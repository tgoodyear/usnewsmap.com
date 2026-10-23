"""Unit tests for ocr/boinc/surya/run.py and Containerfile Vulkan variant (#842)."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("surya_run", HERE / "run.py")
run_mod = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(run_mod)


class SuryaVulkanDetectionTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=HERE.parents[2])
        self.addCleanup(self.tmp.cleanup)
        self.dri = Path(self.tmp.name) / "dri"
        self.dri.mkdir()

    def test_detect_device_reports_vulkan_when_render_node_and_hardware_vulkan_present(self):
        (self.dri / "renderD128").write_text("")
        fake_out = (
            "ggml_vulkan: Found 2 Vulkan devices:\n"
            "Available devices:\n"
            "  Vulkan0: Intel(R) Iris(R) Xe Graphics (TGL GT2) (23950 MiB, 23950 MiB free)\n"
            "  Vulkan1: llvmpipe (LLVM 19.1.7, 256 bits) (32000 MiB, 32000 MiB free)\n"
        )
        cp = mock.Mock(returncode=0, stdout=fake_out, stderr="")
        with mock.patch.object(run_mod.subprocess, "run", return_value=cp) as sub_run:
            devs = run_mod.list_vulkan_devices(llama_binary="/app/llama-server", dri_root=str(self.dri))
            self.assertEqual(
                devs,
                [("Vulkan0", "Intel(R) Iris(R) Xe Graphics (TGL GT2) (23950 MiB, 23950 MiB free)")],
            )
            self.assertEqual(
                run_mod.detect_device("vulkan", llama_binary="/app/llama-server", dri_root=str(self.dri)),
                "vulkan",
            )
            sub_run.assert_called_with(
                ["/app/llama-server", "--list-devices"],
                capture_output=True,
                text=True,
                timeout=15,
                check=False,
            )

    def test_detect_device_reports_cpu_without_render_node_or_with_software_vulkan_only(self):
        # Without /dev/dri/renderD*, llama-server --list-devices is not trusted.
        self.assertEqual(
            run_mod.detect_device("vulkan", llama_binary="/app/llama-server", dri_root=str(self.dri)),
            "cpu",
        )
        (self.dri / "renderD128").write_text("")
        sw_only = "Available devices:\n  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (16000 MiB, 16000 MiB free)\n"
        cp = mock.Mock(returncode=0, stdout=sw_only, stderr="")
        with mock.patch.object(run_mod.subprocess, "run", return_value=cp):
            self.assertEqual(
                run_mod.detect_device("vulkan", llama_binary="/app/llama-server", dri_root=str(self.dri)),
                "cpu",
            )

    def test_detect_device_reports_cuda_via_llama_server_even_when_torch_is_cpu_only(self):
        fake_out = (
            "ggml_cuda_init: found 1 CUDA devices:\n"
            "Available devices:\n"
            "  CUDA0: NVIDIA GeForce RTX 3060 (12044 MiB, 11850 MiB free)\n"
        )
        cp = mock.Mock(returncode=0, stdout=fake_out, stderr="")
        with mock.patch.object(run_mod.subprocess, "run", return_value=cp):
            devs = run_mod.list_cuda_devices(llama_binary="/app/llama-server")
            self.assertEqual(devs, [("CUDA0", "NVIDIA GeForce RTX 3060 (12044 MiB, 11850 MiB free)")])
            self.assertEqual(
                run_mod.detect_device("cuda", llama_binary="/app/llama-server", dri_root=str(self.dri)),
                "cuda",
            )
        no_gpu_cp = mock.Mock(returncode=0, stdout="Available devices:\n", stderr="")
        with mock.patch.object(run_mod.subprocess, "run", return_value=no_gpu_cp):
            self.assertEqual(
                run_mod.detect_device("cuda", llama_binary="/app/llama-server", dri_root=str(self.dri)),
                "cpu",
            )

    def test_containerfile_takes_its_pins_from_engine_lock_and_builds_the_vulkan_variant(self):
        containerfile = (HERE / "Containerfile").read_text()
        lock = dict(
            line.split("=", 1)
            for line in (HERE / "engine.lock").read_text().splitlines()
            if line and not line.startswith("#")
        )
        self.assertEqual(sorted(lock), ["LLAMA_DIGEST", "LLAMA_VULKAN_DIGEST", "SURYA_VERSION", "SURYA_WEIGHTS"])
        self.assertIn("ARG LLAMA_VARIANT=cuda", containerfile)
        for key in lock:
            # An argument with no default: the value is the lock's, passed by the build.
            self.assertRegex(containerfile, rf"(?m)^ARG {key}$")
            self.assertNotRegex(containerfile, rf"(?m)^ARG {key}=")
        self.assertIn("FROM ghcr.io/ggml-org/llama.cpp:server-vulkan@${LLAMA_VULKAN_DIGEST} AS llama-vulkan", containerfile)
        self.assertIn("ENV USNM_SURYA_LLAMA_IMAGE=${LLAMA_VULKAN_DIGEST} USNM_SURYA_BACKEND=vulkan", containerfile)
        self.assertIn('surya-ocr=="$SURYA_VERSION"', containerfile)
        self.assertIn("USNM_SURYA_VERSION=${SURYA_VERSION}", containerfile)
        self.assertIn("TORCH_DEVICE=cpu", containerfile)
        job_intel = (HERE / "job-intel.toml").read_text()
        self.assertIn("use_gpu = false", job_intel)
        self.assertIn("--device=/dev/dri", job_intel)


if __name__ == "__main__":
    unittest.main()
