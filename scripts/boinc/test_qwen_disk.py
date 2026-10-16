"""Disk/model contracts without pulling images, weights, or touching a runtime."""
from __future__ import annotations

import hashlib
import io
import json
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch

from scripts.boinc import qwen_disk as disk
from scripts.boinc import qwen_cold_cache as cold
from scripts.boinc import volunteer_preflight as pre

ROOT = Path(__file__).resolve().parents[2]


class QwenDiskTest(unittest.TestCase):
    def test_committed_ci_observations_fit_policy_and_keep_models_separate(self):
        p = disk.policy()
        observed = json.loads((ROOT / "ocr/boinc/qwen/measurements/ci-38787485639.json").read_text())
        self.assertEqual(set(observed["models"]), {"2b", "8b"})
        self.assertEqual(observed["platform"], "linux/amd64")
        for model, result in observed["models"].items():
            with self.subTest(model=model):
                self.assertTrue(result["measurement_passed"])
                self.assertTrue(result["error_output_shapes_passed"])
                self.assertEqual(result["payload_file_bytes"], p["models"][model]["files"])
                payload = sum(result["payload_file_bytes"].values())
                self.assertLessEqual(result["logical_unpacked_bytes"],
                                     p["base_unpacked_bound_bytes"] + payload + p["image_overhead_bound_bytes"])
                self.assertLessEqual(result["local_gzip_stream_bytes"],
                                     p["base_compressed_bytes"] + payload + p["image_overhead_bound_bytes"])
                self.assertLessEqual(result["peak_filesystem_delta_bytes"] + p["warm_incremental_bytes"] + p["reserve_bytes"],
                                     p["models"][model]["container_cold_bytes"])
                self.assertLessEqual(result["warm_filesystem_delta_bytes"], p["warm_incremental_bytes"])
                self.assertIn(observed["source_revision"], result["image"])
                self.assertEqual(len(result["artifact_archive_sha256"]), 64)
        self.assertNotEqual(observed["models"]["2b"]["image_id"], observed["models"]["8b"]["image_id"])

    def test_cold_daemon_network_and_storage_are_isolated_from_build_daemon(self):
        state = ROOT / ".new-test-store"
        host = f"unix://{state}/docker.sock"
        command = cold.daemon_command(state, host)
        self.assertEqual(command[:5], ["sudo", "-n", "unshare", "--net", "dockerd"])
        for option in (
            f"--data-root={state}/data", f"--exec-root={state}/exec",
            f"--pidfile={state}/daemon.pid", f"--host={host}",
            "--bridge=none", "--iptables=false", "--ip-masq=false",
        ):
            self.assertIn(option, command)

    def test_cold_acceptance_cannot_touch_a_volunteer_or_existing_store(self):
        argv = ["qwen_cold_cache.py", "--image", "cached-only", "--model", "2b", "--state", str(ROOT)]
        with patch("sys.argv", argv), patch.object(cold.subprocess, "run") as run:
            with patch.dict(cold.os.environ, {"GITHUB_ACTIONS": "false"}):
                with self.assertRaisesRegex(SystemExit, "disposable"):
                    cold.main()
            with patch.dict(cold.os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}):
                with self.assertRaisesRegex(SystemExit, "NEW directory"):
                    cold.main()
            run.assert_not_called()

    def test_model_plan_classes_reserve_resources_and_exclude_legacy_versions(self):
        classes = {node.findtext("name"): node for node in ET.parse(ROOT / "contrib-compute/boinc/plan_class_spec.xml").findall("plan_class")}
        cpu = classes["docker_mt_2b"]
        gpu = classes["docker_nvidia_cuda_8b"]
        self.assertEqual((cpu.findtext("min_batch"), cpu.findtext("max_batch")), ("12", "12"))
        self.assertEqual((cpu.findtext("min_ncpus"), cpu.findtext("max_threads")), ("4", "8"))
        self.assertEqual((gpu.findtext("min_batch"), gpu.findtext("max_batch")), ("18", "18"))
        self.assertEqual((gpu.findtext("min_gpu_ram_mb"), gpu.findtext("gpu_ram_used_mb"), gpu.findtext("avg_ncpus")), ("7000", "6500", "2"))
        self.assertEqual(classes["docker_nvidia_cuda"].findtext("min_gpu_ram_mb"), "3000")

    def test_exact_envelopes_account_for_cold_overlap_and_distinct_native_payloads(self):
        p = disk.policy()
        self.assertEqual(p["base_compressed_bytes"], 2592304104)
        self.assertEqual(p["warm_incremental_bytes"], 2147483648)
        for model, payload, container, native in [
            ("2b", 2279480352, 34359738368, 12884901888),
            ("8b", 5780074528, 51539607552, 19327352832),
        ]:
            with self.subTest(model=model):
                m = p["models"][model]
                self.assertEqual(sum(m["files"].values()), payload)
                self.assertEqual(m["container_cold_bytes"], container)
                self.assertEqual(m["native_cold_bytes"], native)
                compressed = p["base_compressed_bytes"] + payload + p["image_overhead_bound_bytes"]
                unpacked = p["base_unpacked_bound_bytes"] + payload + p["image_overhead_bound_bytes"]
                self.assertLessEqual(compressed + 2 * unpacked + p["warm_incremental_bytes"] + p["reserve_bytes"], container)
                self.assertLessEqual(payload + 2 * p["native_bundle_bound_bytes"] + p["warm_incremental_bytes"] + p["reserve_bytes"], native)
                template = ET.parse(ROOT / f"ocr/boinc/usnm_qwen_{model}_in")
                self.assertEqual(int(template.findtext(".//rsc_disk_bound")), native)
                for ref in template.findall(".//file_ref"):
                    if ref.findtext("open_name") in m["files"]:
                        self.assertIsNone(ref.find("copy_file"), "Sticky model aliases must not create another full slot copy")
                job = pre.limits_from_job_toml(ROOT / f"ocr/boinc/qwen/job-{'cpu' if model == '2b' else 'gpu'}.toml")
                self.assertEqual(job.disk_cold_bytes, container)
                self.assertEqual(job.disk_warm_bytes, p["warm_incremental_bytes"])
                self.assertEqual(job.memory, 6 * 1024 ** 3)
                self.assertEqual(job.memory_swap, job.memory)
                self.assertEqual(job.pids, 1024)
                self.assertEqual(job.use_gpu, model == "8b")
        lock = (ROOT / "ocr/boinc/qwen/weights.lock").read_text()
        identity = hashlib.sha256("".join(line + "\n" for line in lock.splitlines() if not line.startswith("#")).encode()).hexdigest()
        self.assertEqual(p["weights_identity"], "sha256:" + identity)
        self.assertEqual(p["native_warm_incremental_bytes"], p["native_bundle_bound_bytes"] + p["warm_incremental_bytes"])
        locked = {line.split()[0] for line in lock.splitlines() if line and not line.startswith("#")} - {"LICENSE"}
        files_2b = set(p["models"]["2b"]["files"])
        files_8b = set(p["models"]["8b"]["files"])
        self.assertFalse(files_2b & files_8b)
        self.assertEqual(files_2b | files_8b, locked)

    def registry(self, model="2b", env_model=None, extra=0, legacy=False):
        p = disk.policy()
        config = {
            "os": "linux", "architecture": "amd64",
            "config": {
                "Labels": {} if legacy else {"org.usnewsmap.qwen.model": model, "org.usnewsmap.qwen.disk-policy": "1"},
                "Env": [f"USNM_QWEN_MODEL={env_model or model}", f"USNM_QWEN_LLAMA_IMAGE={p['base_index']}",
                        f"USNM_QWEN_WEIGHTS={p['weights_identity']}"],
            },
        }
        blobs = {}

        def store(value):
            raw = json.dumps(value).encode()
            digest = "sha256:" + hashlib.sha256(raw).hexdigest()
            blobs[digest] = raw
            return digest

        config_digest = store(config)
        limit = p["base_compressed_bytes"] + sum(p["models"][model]["files"].values()) + p["image_overhead_bound_bytes"]
        manifest = store({"config": {"digest": config_digest},
                          "layers": [{"size": limit + extra, "digest": "sha256:" + "a" * 64}]})
        index = store({"manifests": [{"platform": {"os": "linux", "architecture": "amd64"}, "digest": manifest}]})
        calls = []

        def urlopen(request, timeout):
            url = request if isinstance(request, str) else request.full_url
            calls.append(url)
            if url.startswith("https://ghcr.io/token?"):
                return io.BytesIO(b'{"token":"anonymous-test-token"}')
            return io.BytesIO(blobs[url.rsplit("/", 1)[-1]])

        return disk.REPOSITORY + "@" + index, urlopen, calls, blobs

    def test_anonymous_metadata_is_pinned_deterministic_and_never_fetches_layers(self):
        for model in ("2b", "8b"):
            image, urlopen, calls, _ = self.registry(model)
            with patch.object(disk.urllib.request, "urlopen", side_effect=urlopen):
                a = disk.image_metadata(image, model)
                self.assertEqual(a, disk.image_metadata(image, model))
            self.assertEqual(a["model"], model)
            self.assertEqual(len(set(calls)), 4)
            self.assertEqual(len([url for url in calls if "/blobs/" in url]), 2)  # config only, twice

    def test_refuses_wrong_payload_legacy_metadata_tampering_and_oversize(self):
        for model, env_model, extra, legacy, expected in [
            ("2b", None, 0, False, "8b"),
            ("2b", "8b", 0, False, "2b"),
            ("8b", None, 1, False, "8b"),
            ("2b", None, 0, True, None),
        ]:
            image, urlopen, _, _ = self.registry(model, env_model, extra, legacy)
            with patch.object(disk.urllib.request, "urlopen", side_effect=urlopen), self.assertRaises(ValueError):
                disk.image_metadata(image, expected)
        image, urlopen, _, blobs = self.registry()
        blobs[image.split("@")[1]] += b" "
        with patch.object(disk.urllib.request, "urlopen", side_effect=urlopen), self.assertRaisesRegex(ValueError, "digest-mismatched"):
            disk.image_metadata(image, "2b")
        for image in (disk.REPOSITORY + ":latest", "private.example/qwen@sha256:" + "a" * 64):
            with self.assertRaises(ValueError):
                disk.image_metadata(image, "2b")


if __name__ == "__main__":
    unittest.main()
