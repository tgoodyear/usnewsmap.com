"""Host contract regressions; no registry, system changes or account credentials."""
from __future__ import annotations

import importlib.util
import json
from collections import namedtuple
import shlex
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


pre = load("volunteer_preflight")
setup = load("setup_volunteer")
IMAGE = "registry.example.org/usnm/worker@sha256:" + "a" * 64
LIMITS = {"cpu": "100000 100000", "memory": str(pre.MEMORY), "swap": "0", "pids": "64", "ready": "yes"}
INFO = {"host": {"security": {"rootless": True}, "cgroupVersion": "v2", "cgroupManager": "systemd"}}


class Runtime:
    def __init__(self, values=None, slot_error=False, pause_error=False, cleanup_error=False, hello_error=False):
        self.values = values if values is not None else LIMITS
        self.slot_error = slot_error
        self.pause_error = pause_error
        self.cleanup_error = cleanup_error
        self.hello_error = hello_error
        self.calls = []
        self.status = "running"

    def __call__(self, args, timeout=30):
        self.calls.append(args)
        if args[1] == "info":
            return json.dumps(dict(INFO, store={"graphRoot": str(Path(tempfile.gettempdir()) / "fake-images"),
                                               "imageCopyTmpDir": tempfile.gettempdir()}))
        if args[1] == "run" and self.hello_error:
            raise pre.PreflightError("Cannot run podman: timed out")
        if args[1] == "create":
            self.scratch = Path(args[args.index("--volume") + 1].removesuffix(":/work"))
            if not self.slot_error:
                output = self.scratch / "output"
                output.write_text("slot readable and writable\n")
                output.chmod(0o644)
        if args[1] == "logs":
            if self.slot_error:
                return "cat: /work/input: Permission denied\n"
            return "\n".join(f"{key}={value}" for key, value in self.values.items())
        if args[1] == "inspect":
            return json.dumps([{"State": {"Status": "exited" if self.slot_error else self.status}}])
        if args[1] == "pause":
            if self.pause_error:
                raise pre.PreflightError("cannot pause container without a cgroup")
            self.status = "paused"
        if args[1] == "unpause":
            self.status = "running"
        if args[1] == "rm" and self.cleanup_error:
            raise pre.PreflightError("runtime cleanup unavailable")
        return ""


class PreflightTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.data = Path(self.tmp.name)
        (self.data / "slots").mkdir()
        self.uid = patch.object(pre.os, "geteuid", return_value=1000)
        self.uid.start()
        usage = namedtuple("Usage", "total used free")
        self.disk_usage = patch.object(
            pre.shutil, "disk_usage",
            return_value=usage(128 * 1024 ** 3, 64 * 1024 ** 3, 64 * 1024 ** 3),
        )
        self.disk_usage.start()
        self.addCleanup(self.disk_usage.stop)

    def tearDown(self):
        self.uid.stop()
        self.tmp.cleanup()

    def test_bounded_host_checks_real_slot_and_cleans_only_its_probe(self):
        other = self.data / "slots" / "existing-work"
        other.mkdir()
        runtime = Runtime()
        result = pre.preflight(self.data, IMAGE, runtime)
        self.assertTrue(result["pause_resume"])
        self.assertEqual(list((self.data / "slots").iterdir()), [other])
        create = next(call for call in runtime.calls if call[1] == "create")
        self.assertNotIn(":Z", " ".join(create))
        self.assertNotIn(":z", " ".join(create))
        self.assertIn("--network=none", create)
        self.assertIn("--read-only", create)
        self.assertEqual([call[1] for call in runtime.calls if call[1] in ["pause", "unpause", "rm"]], ["rm", "pause", "unpause", "rm"])

    def test_hello_world_timeout_cleans_up_named_probe(self):
        runtime = Runtime(hello_error=True)
        with self.assertRaisesRegex(pre.PreflightError, "timed out"):
            pre.preflight(self.data, IMAGE, runtime)
        run_call = next(call for call in runtime.calls if call[1] == "run")
        hello_name = run_call[run_call.index("--name") + 1]
        self.assertIn(["podman", "rm", "--force", "--ignore", hello_name], runtime.calls)

    def test_disk_admission_is_before_any_pull_or_probe_even_with_cached_image(self):
        usage = namedtuple("Usage", "total used free")
        required = 34359738368
        job = Path(__file__).resolve().parents[2] / "ocr/boinc/qwen/job-cpu.toml"
        runtime = Runtime()
        with patch.object(pre.shutil, "disk_usage", return_value=usage(required, 1, required - 1)):
            with self.assertRaisesRegex(pre.PreflightError, "34359738368 cold-cache bytes"):
                pre.preflight(self.data, IMAGE, runtime, job_toml=job)
        self.assertEqual([call[1] for call in runtime.calls], ["info"])
        with patch.object(pre.shutil, "disk_usage", return_value=usage(required, 0, required)):
            self.assertTrue(pre.check_disk([self.data], required))
        # A different image store/scratch filesystem cannot hide behind the slot's free space.
        with patch.object(pre.shutil, "disk_usage", side_effect=[usage(required, 0, required), usage(required, 1, required - 1)]):
            with self.assertRaises(pre.PreflightError):
                pre.check_disk([self.data / "slots", self.data], required)
        with patch.object(pre, "filesystem_id", side_effect=[1, 2]):
            with self.assertRaisesRegex(pre.PreflightError, "share BOINC's filesystem"):
                pre.check_disk([self.data / "slots", self.data], required, co_located=True)

    def test_qwen_metadata_enforces_cold_disk_even_without_an_explicit_job(self):
        runtime = Runtime()
        with patch.object(pre.qwen_disk, "image_metadata", return_value={"container_cold_bytes": 51539607552}), \
                patch.object(pre, "check_disk", side_effect=pre.PreflightError("Insufficient free disk")) as check:
            with self.assertRaisesRegex(pre.PreflightError, "Insufficient"):
                pre.preflight(self.data, pre.qwen_disk.REPOSITORY + "@sha256:" + "a" * 64, runtime)
        self.assertEqual(check.call_args.args[1], 51539607552)
        self.assertEqual([call[1] for call in runtime.calls], ["info"])

    def test_each_qwen_filesystem_has_deterministic_exact_boundary_admission(self):
        usage = namedtuple("Usage", "total used free")
        paths = [self.data / "slots", self.data / "image-store", self.data / "extraction"]
        for required in (34359738368, 51539607552):
            sufficient = usage(required, 0, required)
            with self.subTest(required=required, case="exact"):
                with patch.object(pre.shutil, "disk_usage", return_value=sufficient):
                    self.assertTrue(pre.check_disk(paths, required, co_located=True))
            for deficient in range(len(paths)):
                values = [sufficient] * len(paths)
                values[deficient] = usage(required, 1, required - 1)
                with self.subTest(required=required, deficient=deficient):
                    with patch.object(pre.shutil, "disk_usage", side_effect=values):
                        with self.assertRaisesRegex(pre.PreflightError, "Insufficient free disk"):
                            pre.check_disk(paths, required, co_located=True)

    def test_qwen_cannot_admit_unknown_storage_or_malformed_disk_markers(self):
        source = Path(__file__).resolve().parents[2] / "ocr/boinc/qwen/job-cpu.toml"
        runtime = Runtime()

        def missing_store(args, timeout=30):
            return json.dumps(INFO) if args[1] == "info" else runtime(args, timeout)

        with self.assertRaisesRegex(pre.PreflightError, "did not report absolute"):
            pre.preflight(self.data, IMAGE, missing_store, job_toml=source)
        self.assertFalse(runtime.calls)
        job = self.data / "job.toml"
        for raw in [
            source.read_text() + "\n# usnm_disk_cold_bytes=1\n",
            source.read_text().replace("# usnm_disk_warm_bytes=2147483648", ""),
            source.read_text().replace("# usnm_disk_cold_bytes=34359738368", "# usnm_disk_cold_bytes=0"),
            source.read_text().replace("# usnm_disk_cold_bytes=34359738368", "# usnm_disk_cold_bytes=1"),
        ]:
            job.write_text(raw)
            with self.assertRaises(pre.PreflightError):
                pre.limits_from_job_toml(job)

    def test_probe_keeps_the_released_wrapper_runtime_policy(self):
        runtime = Runtime()
        pre.preflight(self.data, IMAGE, runtime)
        create = next(call for call in runtime.calls if call[1] == "create")
        job = pre._load_toml((Path(__file__).resolve().parents[2] / "ocr/boinc/job.toml").read_text())
        for option in shlex.split(str(job["create_args"])):
            self.assertIn(option, create)
        self.assertEqual(job["workdir"], "/work")

    def test_cli_success_with_ignored_kernel_limits_is_rejected(self):
        for key, ignored in [("cpu", "max 100000"), ("memory", "max"), ("swap", "max"), ("pids", "max"), ("pids", "199632")]:
            with self.subTest(key=key):
                runtime = Runtime(dict(LIMITS, **{key: ignored}))
                with self.assertRaises(pre.PreflightError):
                    pre.preflight(self.data, IMAGE, runtime)
                self.assertTrue(any(call[1] == "rm" for call in runtime.calls))
                self.assertFalse(any(call[1] == "pause" for call in runtime.calls))

    def test_selinux_slot_failure_is_actionable_and_cleans_up(self):
        runtime = Runtime(slot_error=True)
        with self.assertRaisesRegex(pre.PreflightError, "container_file_t"):
            pre.preflight(self.data, IMAGE, runtime)
        self.assertEqual(list((self.data / "slots").iterdir()), [])

    def test_pause_failure_and_cleanup_failure_are_fatal(self):
        for runtime in [Runtime(pause_error=True), Runtime(cleanup_error=True)]:
            with self.assertRaises(pre.PreflightError):
                pre.preflight(self.data, IMAGE, runtime)

    def test_rootful_v1_or_cgroupfs_runtime_cannot_pass(self):
        for host in [dict(INFO["host"], security={"rootless": False}), dict(INFO["host"], cgroupVersion="v1"), dict(INFO["host"], cgroupManager="cgroupfs")]:
            with self.assertRaises(pre.PreflightError):
                pre.check_info({"host": host})

    def test_unpinned_image_never_calls_runtime(self):
        runtime = Runtime()
        for image in ["worker:latest", IMAGE + "\nRUN bad", "sha256:" + "a" * 64]:
            with self.assertRaises(pre.PreflightError):
                pre.preflight(self.data, image, runtime)
        self.assertEqual(runtime.calls, [])

    def test_service_environment_override_or_removal_fails_before_podman(self):
        service = "boinc-client.service"
        expected = {
            "HOME": str(self.data),
            "XDG_RUNTIME_DIR": "/run/user/1000/usnm-boinc-boinc-client",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
            "CONTAINERS_CONF": f"/etc/usnm-boinc-volunteer/{service}/containers.conf",
            "CONTAINERS_STORAGE_CONF": f"/etc/usnm-boinc-volunteer/{service}/storage.conf",
        }
        with patch.dict(pre.os.environ, expected, clear=True):
            self.assertTrue(pre.preflight(self.data, IMAGE, Runtime(), service=service)["rootless"])
        for key in ["XDG_RUNTIME_DIR", "CONTAINERS_STORAGE_CONF"]:
            overridden = dict(expected, **{key: "/var/lib/unmanaged"})
            runtime = Runtime()
            with patch.dict(pre.os.environ, overridden, clear=True):
                with self.assertRaises(pre.PreflightError):
                    pre.preflight(self.data, IMAGE, runtime, service=service)
            self.assertEqual(runtime.calls, [])
            removed = {k: v for k, v in expected.items() if k != key}
            with patch.dict(pre.os.environ, removed, clear=True):
                with self.assertRaises(pre.PreflightError):
                    pre.preflight(self.data, IMAGE, runtime, service=service)
            self.assertEqual(runtime.calls, [])

    def test_limits_from_job_toml_parses_ocr_surya_and_qwen_jobs(self):
        repo = Path(__file__).resolve().parents[2]
        ocr_limits = pre.limits_from_job_toml(repo / "ocr/boinc/job.toml")
        self.assertEqual(ocr_limits.workdir, "/work")
        self.assertEqual(ocr_limits.memory, 1536 * 1024 * 1024)
        self.assertEqual(ocr_limits.pids, 64)
        self.assertEqual(ocr_limits.cpus, 1)
        self.assertFalse(ocr_limits.use_gpu)

        qwen_cpu = pre.limits_from_job_toml(repo / "ocr/boinc/qwen/job-cpu.toml")
        self.assertEqual(qwen_cpu.memory, 6144 * 1024 * 1024)
        self.assertEqual(qwen_cpu.pids, 1024)
        self.assertFalse(qwen_cpu.use_gpu)

        surya_cpu = pre.limits_from_job_toml(repo / "ocr/boinc/surya/job-cpu.toml")
        self.assertEqual(surya_cpu.memory, 12 * 1024 ** 3)
        self.assertEqual(surya_cpu.pids, 1024)
        self.assertFalse(surya_cpu.use_gpu)

        for gpu_path in [repo / "ocr/boinc/surya/job-gpu.toml", repo / "ocr/boinc/qwen/job-gpu.toml"]:
            gpu_limits = pre.limits_from_job_toml(gpu_path)
            self.assertTrue(gpu_limits.use_gpu)
            self.assertEqual(gpu_limits.pids, 1024)

    def test_preflight_runs_with_job_toml_and_discovers_project_jobs(self):
        repo = Path(__file__).resolve().parents[2]
        qwen_cpu_path = repo / "ocr/boinc/qwen/job-cpu.toml"
        qwen_values = {
            "cpu": "100000 100000",
            "memory": str(6144 * 1024 * 1024),
            "swap": "0",
            "pids": "1024",
            "ready": "yes",
        }
        runtime = Runtime(qwen_values)
        result = pre.preflight(self.data, IMAGE, runtime, job_toml=qwen_cpu_path)
        self.assertEqual(result["kernel_limits"]["memory"], str(6144 * 1024 * 1024))
        create = next(call for call in runtime.calls if call[1] == "create")
        self.assertIn("--memory=6g", create)
        self.assertIn("--pids-limit=1024", create)
        self.assertIn("--cpus=1", create)

        # Also test auto-discovery from --data-dir/projects/**/job*.toml
        proj_dir = self.data / "projects" / "boinc.usnewsmap.com_usnmocr"
        proj_dir.mkdir(parents=True)
        (proj_dir / "job_qwen.toml").write_text(qwen_cpu_path.read_text())
        runtime_auto = Runtime(qwen_values)
        pre.preflight(self.data, IMAGE, runtime_auto)
        create_auto = next(call for call in runtime_auto.calls if call[1] == "create")
        self.assertIn("--memory=6g", create_auto)
        self.assertIn("--pids-limit=1024", create_auto)

    def test_preflight_gpu_job_passes_gpus_all_and_reports_gpu_failure(self):
        repo = Path(__file__).resolve().parents[2]
        qwen_gpu_path = repo / "ocr/boinc/qwen/job-gpu.toml"
        gpu_values = {
            "cpu": "100000 100000",
            "memory": str(6144 * 1024 * 1024),
            "swap": "0",
            "pids": "1024",
            "ready": "yes",
        }
        runtime = Runtime(gpu_values)
        pre.preflight(self.data, IMAGE, runtime, job_toml=qwen_gpu_path)
        create = next(call for call in runtime.calls if call[1] == "create")
        self.assertIn("--gpus", create)
        self.assertIn("all", create)

        def failing_gpu_runner(args, timeout=30):
            if args[1] == "create":
                raise pre.PreflightError("podman create failed: CDI device nvidia.com/gpu=all not found")
            return runtime(args, timeout=timeout)

        with self.assertRaisesRegex(pre.PreflightError, "nvidia-ctk cdi generate"):
            pre.preflight(self.data, IMAGE, failing_gpu_runner, job_toml=qwen_gpu_path)

    def test_invalid_job_toml_is_rejected(self):
        bad_toml = self.data / "bad_job.toml"
        for content in [
            'workdir = "relative"\ncreate_args = "--network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=1536m --memory-swap=1536m --pids-limit=64"\n',
            'workdir = "/work"\ncreate_args = "--read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=1536m --memory-swap=1536m --pids-limit=64"\n',
            'workdir = "/work"\ncreate_args = "--network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=1536m --memory-swap=2048m --pids-limit=64"\n',
        ]:
            bad_toml.write_text(content)
            with self.assertRaises(pre.PreflightError):
                pre.limits_from_job_toml(bad_toml)

    def test_python39_toml_fallback_parses_released_job_files_and_rejects_malformed_toml(self):
        repo = Path(__file__).resolve().parents[2]
        with patch.object(pre, "tomllib", None):
            ocr_limits = pre.limits_from_job_toml(repo / "ocr/boinc/job.toml")
            self.assertEqual(ocr_limits.workdir, "/work")
            self.assertEqual(ocr_limits.memory, 1536 * 1024 * 1024)
            self.assertEqual(ocr_limits.pids, 64)
            self.assertFalse(ocr_limits.use_gpu)

            surya_cpu = pre.limits_from_job_toml(repo / "ocr/boinc/surya/job-cpu.toml")
            self.assertEqual(surya_cpu.memory, 12 * 1024 ** 3)
            self.assertEqual(surya_cpu.pids, 1024)
            self.assertFalse(surya_cpu.use_gpu)

            qwen_gpu = pre.limits_from_job_toml(repo / "ocr/boinc/qwen/job-gpu.toml")
            self.assertEqual(qwen_gpu.memory, 6 * 1024 ** 3)
            self.assertEqual(qwen_gpu.pids, 1024)
            self.assertTrue(qwen_gpu.use_gpu)

            multiline_toml = self.data / "multiline_job.toml"
            multiline_toml.write_text(
                '# comment with "quotes"\n'
                'workdir = "/work" # inline comment\n'
                "use_gpu = false\n"
                "create_args = [\n"
                '  "--network=none",\n'
                '  "--read-only",\n'
                '  "--cap-drop=ALL",\n'
                '  "--security-opt=no-new-privileges",\n'
                '  "--memory=2048m",\n'
                '  "--memory-swap=2048m",\n'
                '  "--pids-limit=128",\n'
                "]\n"
            )
            multiline_limits = pre.limits_from_job_toml(multiline_toml)
            self.assertEqual(multiline_limits.memory, 2048 * 1024 * 1024)
            self.assertEqual(multiline_limits.pids, 128)

            bad_toml = self.data / "malformed.toml"
            for bad_content in [
                'workdir = "/work\n',
                "not_an_assignment\n",
                "[section]\nworkdir = \"/work\"\n",
                'workdir = "/work"\nworkdir = "/work"\n',
                'workdir = "/work"\ncreate_args = ["unterminated"\n',
            ]:
                bad_toml.write_text(bad_content)
                with self.assertRaises(pre.PreflightError):
                    pre.limits_from_job_toml(bad_toml)


class SetupTest(unittest.TestCase):
    def test_overlapping_subordinate_ids_are_rejected_without_reallocation(self):
        setup.check_subids("boinc:100000:65536\nother:200000:65536\n", "boinc", 999)
        setup.check_subids("999:100000:65536\n", "boinc", 999)
        for text in ["boinc:100000:65536\nother:100000:65536\n", "boinc:100000:65536\nother:160000:65536\n", "boinc:100000:1024\n", "other:100000:65536\n"]:
            with self.assertRaises(setup.SetupError):
                setup.check_subids(text, "boinc", 999)

    def test_unsafe_cli_values_fail_before_reading_service_or_files(self):
        with patch.object(setup, "run") as commands:
            for service, data, image in [("x.service\nOther=1", Path("/var/lib/boinc"), IMAGE), ("boinc-client.service", Path("/var/lib/boinc space"), IMAGE), ("boinc-client.service", Path("/var/lib/boinc"), IMAGE + "\nOther=1")]:
                with self.assertRaises(setup.SetupError):
                    setup.setup(service, data, image)
            commands.assert_not_called()

    def test_queued_or_unreported_work_blocks_reconfiguration(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)
            self.assertFalse(setup.active_work(path))
            for xml in ["<client_state><result><state>1</state></result></client_state>", "<client_state><result><ready_to_report/></result></client_state>", "<client_state><active_task_set><active_task/></active_task_set></client_state>"]:
                (path / "client_state.xml").write_text(xml)
                self.assertTrue(setup.active_work(path))

    def test_always_run_activity_mode_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)
            setup.check_activity_mode(path)
            (path / "client_state.xml").write_text("<client_state><user_run_request>2</user_run_request><user_gpu_request>2</user_gpu_request></client_state>")
            setup.check_activity_mode(path)
            for xml in [
                "<client_state><user_run_request>1</user_run_request></client_state>",
                "<client_state><user_run_request>3</user_run_request></client_state>",
                "<client_state><user_run_request>2</user_run_request><user_gpu_request>1</user_gpu_request></client_state>",
            ]:
                (path / "client_state.xml").write_text(xml)
                with self.assertRaises(setup.SetupError):
                    setup.check_activity_mode(path)

    def test_prepare_slots_rejects_symlink_and_pins_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "boinc"
            data.mkdir()
            outside = Path(tmp) / "outside"
            outside.mkdir()
            uid = setup.os.getuid()
            gid = setup.os.getgid()
            with patch.object(setup.os, "fchown") as fchown:
                setup.prepare_slots(data, uid, gid, trust=outside / "ca.crt")
                self.assertTrue((data / "slots").is_dir())
                self.assertFalse((data / "slots").is_symlink())
                self.assertTrue((data / "ca-bundle.crt").is_symlink())
                fchown.assert_called_once()
            (data / "slots").rmdir()
            (data / "slots").symlink_to(outside)
            with self.assertRaises(setup.SetupError):
                setup.prepare_slots(data, uid, gid)

    def test_preferences_keep_unrelated_settings_and_do_not_touch_account(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prefs.xml"
            path.write_text("<global_preferences><disk_max_used_gb>12</disk_max_used_gb><run_if_user_active>1</run_if_user_active></global_preferences>")
            result = ET.fromstring(setup.merge_xml(path, "global_preferences", setup.PREFS))
            self.assertEqual(result.findtext("disk_max_used_gb"), "12")
            self.assertEqual(result.findtext("run_if_user_active"), "0")
            self.assertEqual(result.findtext("leave_apps_in_memory"), "1")
            self.assertEqual(result.findtext("suspend_cpu_usage"), "0")

    def test_managed_dropin_preflights_inside_service_and_preserves_runtime(self):
        _, dropin, config, storage = setup.profile("boinc-client.service", Path("/var/lib/boinc"), IMAGE, 999)
        self.assertIn("Requires=user@999.service", dropin)
        self.assertIn("After=user@999.service", dropin)
        self.assertIn("ExecStartPre=/usr/bin/python3 /usr/local/lib/usnm-boinc-volunteer/preflight.py --data-dir=/var/lib/boinc --image=" + IMAGE + " --service=boinc-client.service", dropin)
        self.assertIn("RuntimeDirectoryPreserve=yes", dropin)
        self.assertIn("RuntimeDirectory=user/999/usnm-boinc-boinc-client\n", dropin)
        self.assertIn("Environment=XDG_RUNTIME_DIR=/run/user/999/usnm-boinc-boinc-client\n", dropin)
        self.assertIn('runroot="/run/user/999/usnm-boinc-boinc-client/containers"', storage)
        self.assertIn("ProtectControlGroups=false", dropin)
        self.assertIn("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/999/bus", dropin)
        self.assertIn('conmon_cgroup="pod"', config)
        self.assertNotIn("docker", dropin)
        _, dropin_jobs, _, _ = setup.profile(
            "boinc-client.service",
            Path("/var/lib/boinc"),
            IMAGE,
            999,
            job_tomls=[Path("/etc/usnm-boinc-volunteer/job-cpu.toml")],
        )
        self.assertIn("--job-toml=/etc/usnm-boinc-volunteer/job-cpu.toml", dropin_jobs)

    def test_profiles_keep_runtime_paths_under_each_users_apparmor_allowed_tree(self):
        for uid in (999, 1000):
            for service in ("boinc-client.service", "boinc-pilot.service"):
                _, dropin, _, storage = setup.profile(service, Path("/var/lib/boinc"), IMAGE, uid)
                runtime = f"user/{uid}/usnm-boinc-{service.removesuffix('.service')}"
                self.assertIn(f"Environment=XDG_RUNTIME_DIR=/run/{runtime}\n", dropin)
                self.assertIn(f"RuntimeDirectory={runtime}\n", dropin)
                self.assertIn('RuntimeDirectoryMode=0700\n', dropin)
                self.assertIn(f'runroot="/run/{runtime}/containers"', storage)

    def test_optional_ocr_profile_caps_the_entire_dedicated_identity(self):
        setup.check_ocr_identity("boinc", 999, [1000, 1001])
        for user, uid in [("usnm", 999), ("boinc", 1000), ("boinc", 0)]:
            with self.assertRaises(setup.SetupError):
                setup.check_ocr_identity(user, uid, [1000, 1001])
        _, dropin, _, _ = setup.profile(
            "boinc-client.service", Path("/var/lib/boinc"), IMAGE, 999, ocr_node=True)
        self.assertIn("Slice=user-999.slice", dropin)
        self.assertIn("--ocr-node-uid=999", dropin)
        self.assertIn("Nice=19", dropin)
        self.assertIn("IOSchedulingClass=idle", dropin)
        self.assertIn("CPUQuota=25%", setup.OCR_NODE_SLICE)
        self.assertIn("MemoryMax=2G", setup.OCR_NODE_SLICE)
        self.assertIn("MemorySwapMax=0", setup.OCR_NODE_SLICE)
        _, normal, _, _ = setup.profile("boinc-client.service", Path("/var/lib/boinc"), IMAGE, 999)
        self.assertNotIn("Slice=", normal)

    def test_ocr_group_checks_include_account_service_and_user_manager_grants(self):
        account = namedtuple("Account", "pw_name pw_gid")("boinc", 999)
        group = namedtuple("Group", "gr_gid")
        mapping = {"boinc": 999, "root": 0, "usnmocr": 601, "himds": 602}

        def resolve(name):
            if name not in mapping:
                raise KeyError(name)
            return group(mapping[name])

        with patch.object(pre.pwd, "getpwuid", return_value=account), \
             patch.object(pre.os, "getgrouplist", return_value=[999]) as memberships, \
             patch.object(pre.grp, "getgrnam", side_effect=resolve), \
             patch.object(pre.os, "getgroups", return_value=[999, 602]), \
             patch.object(pre.os, "getegid", return_value=999):
            pre.check_ocr_groups(999, "boinc-client.service", runner=lambda _: "")
            for unit in ["boinc-client.service", "user@999.service"]:
                for prop, value in [("Group", "usnmocr"), ("SupplementaryGroups", "601"),
                                    ("SupplementaryGroups", "himds")]:
                    def runner(args):
                        return value if args[2] == unit and args[3] == f"--property={prop}" else ""
                    with self.assertRaises(pre.PreflightError):
                        pre.check_ocr_groups(999, "boinc-client.service", runner=runner)
            memberships.return_value = [999, 602]
            with self.assertRaisesRegex(pre.PreflightError, "himds"):
                pre.check_ocr_groups(999, "boinc-client.service", runner=lambda _: "")
            memberships.return_value = [999]
            with self.assertRaisesRegex(pre.PreflightError, "himds"):
                pre.check_ocr_groups(999, "boinc-client.service", runner=lambda _: "", actual=True)

    def test_ocr_preflight_reads_aggregate_kernel_limits_not_container_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parent = root / "user.slice/user-999.slice"
            parent.mkdir(parents=True)
            values = {"cpu.max": "25000 100000", "memory.max": str(2 * 1024**3),
                      "memory.swap.max": "0", "pids.max": "256"}
            membership = "0::/user.slice/user-999.slice/boinc-client.service"
            for name, value in values.items():
                (parent / name).write_text(value)
            with patch.object(pre.os, "geteuid", return_value=999):
                pre.check_ocr_node_slice(999, root, membership)
                for name, bad in [("cpu.max", "max 100000"), ("cpu.max", "50000 100000"),
                                  ("memory.max", "max"), ("memory.swap.max", "1024"),
                                  ("pids.max", "max")]:
                    (parent / name).write_text(bad)
                    with self.assertRaises(pre.PreflightError):
                        pre.check_ocr_node_slice(999, root, membership)
                    (parent / name).write_text(values[name])
                with self.assertRaises(pre.PreflightError):
                    pre.check_ocr_node_slice(999, root, "0::/system.slice/boinc-client.service")

    def test_conflicting_later_dropin_cannot_silently_bypass_profile(self):
        service = "boinc-client.service"
        data = Path("/var/lib/boinc")
        config, _, _, _ = setup.profile(service, data, IMAGE, 999)
        environment = {
            "HOME": str(data), "XDG_RUNTIME_DIR": "/run/user/999/usnm-boinc-boinc-client",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/999/bus",
            "CONTAINERS_CONF": str(config / "containers.conf"),
            "CONTAINERS_STORAGE_CONF": str(config / "storage.conf"),
        }
        valid_start = "{ path=/usr/bin/boinc ; argv[]=/usr/bin/boinc --dir ${BOINC_DIR} $BOINC_OPTS ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        optional_pre = "{ path=/bin/chown ; argv[]=/bin/chown boinc:boinc /var/lib/boinc ; ignore_errors=yes ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        valid_pre = (
            "{ path=/usr/bin/python3 ; argv[]=/usr/bin/python3 /usr/local/lib/usnm-boinc-volunteer/preflight.py "
            f"--data-dir={data} --image={IMAGE} --service={service} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}"
        )
        with tempfile.TemporaryDirectory() as tmp:
            env_dir = Path(tmp)
            harmless_env = env_dir / "harmless.env"
            harmless_env.write_text("# comment\nBOINC_DIR=/var/lib/boinc\nBOINC_OPTS=\"--dir /var/lib/boinc\"\n")
            override_dir_env = env_dir / "override-dir.env"
            override_dir_env.write_text("BOINC_OPTS=\"--dir /var/lib/other-boinc\"\n")
            override_boinc_dir = env_dir / "override-boinc-dir.env"
            override_boinc_dir.write_text("BOINC_DIR=/var/lib/other-boinc\n")
            override_runtime = env_dir / "override-runtime.env"
            override_runtime.write_text("XDG_RUNTIME_DIR=/run/unmanaged\n")
            override_storage = env_dir / "override-storage.env"
            override_storage.write_text("export CONTAINERS_STORAGE_CONF=\"/etc/containers/storage.conf\"\n")
            missing_env = env_dir / "missing.env"
            base_env_str = " ".join(f"{k}={v}" for k, v in environment.items())
            properties = {
                "Requires": "basic.target user@999.service",
                "After": "network.target user@999.service",
                "WorkingDirectory": str(data),
                "ExecStart": valid_start,
                "Environment": base_env_str,
                "UnsetEnvironment": "UNRELATED_VAR",
                "EnvironmentFiles": f"{harmless_env} (ignore_errors=no) {missing_env} (ignore_errors=yes)",
                "ProtectControlGroups": "no",
                "RuntimeDirectoryPreserve": "yes",
                "ExecStartPre": f"{optional_pre}\n{valid_pre}",
            }
            def command(args, timeout=180):
                key = next(item.removeprefix("--property=") for item in args if item.startswith("--property="))
                return properties[key]
            with patch.object(setup, "run", side_effect=command):
                setup.check_effective(service, data, config, 999, IMAGE)
                # Conflicting BOINC_OPTS: Environment has /var/lib/boinc, EnvironmentFile overrides it with /var/lib/other-boinc.
                properties["Environment"] = f'{base_env_str} "BOINC_OPTS=--dir /var/lib/boinc"'
                properties["EnvironmentFiles"] = f"{harmless_env} (ignore_errors=no) {override_dir_env} (ignore_errors=yes)"
                with self.assertRaises(setup.SetupError):
                    setup.check_effective(service, data, config, 999, IMAGE)
                properties["Environment"] = base_env_str
                properties["EnvironmentFiles"] = f"{harmless_env} (ignore_errors=no) {missing_env} (ignore_errors=yes)"
                bad_cases = [
                    ("Requires", "basic.target"),
                    ("After", "network.target"),
                    ("WorkingDirectory", "/var/lib/other-boinc"),
                    ("ExecStart", valid_start.replace("$BOINC_OPTS", "--dir /var/lib/other-boinc")),
                    ("ExecStart", valid_start.replace("$BOINC_OPTS", "--dir=/var/lib/other-boinc")),
                    ("ExecStart", valid_start.replace("$BOINC_OPTS", "--dir")),
                    ("EnvironmentFiles", f"{override_dir_env} (ignore_errors=no)"),
                    ("EnvironmentFiles", f"{override_boinc_dir} (ignore_errors=no)"),
                    ("EnvironmentFiles", f"{harmless_env}"),
                    ("Environment", "HOME=/old"),
                    ("UnsetEnvironment", "XDG_RUNTIME_DIR"),
                    ("UnsetEnvironment", f"CONTAINERS_STORAGE_CONF={config / 'storage.conf'}"),
                    ("EnvironmentFiles", f"{override_runtime} (ignore_errors=yes)"),
                    ("EnvironmentFiles", f"{override_storage} (ignore_errors=no)"),
                    ("EnvironmentFiles", f"{missing_env} (ignore_errors=no)"),
                    ("ProtectControlGroups", "yes"),
                    ("RuntimeDirectoryPreserve", "no"),
                    ("ExecStartPre", ""),
                    ("ExecStartPre", optional_pre),
                    ("ExecStartPre", valid_pre.replace("ignore_errors=no", "ignore_errors=yes")),
                    ("ExecStartPre", valid_pre.replace("path=/usr/bin/python3", "path=/bin/true")),
                    ("ExecStartPre", valid_pre.replace(f"--service={service}", "--service=other.service")),
                    ("ExecStartPre", valid_pre.replace(f"--image={IMAGE}", "--image=registry.example.org/usnm/worker@sha256:" + "b" * 64)),
                ]
                for key, bad in bad_cases:
                    with self.subTest(key=key, bad=bad):
                        original = properties[key]
                        properties[key] = bad
                        with self.assertRaises(setup.SetupError):
                            setup.check_effective(service, data, config, 999, IMAGE)
                        properties[key] = original

    def test_backup_records_initial_absence_and_preserves_original_across_reapply(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            data = root / "data"
            backups = root / "backups"
            data.mkdir()
            backups.mkdir()
            cc = data / "cc_config.xml"
            prefs = data / "global_prefs_override.xml"
            original_prefs = "<global_preferences><disk_max_used_gb>12</disk_max_used_gb></global_preferences>\n"
            prefs.write_text(original_prefs)
            with patch.object(setup.os, "fchown"):
                setup.backup(cc, backups)
                setup.backup(prefs, backups)
                self.assertTrue((backups / "cc_config.xml.absent").exists())
                self.assertFalse((backups / "cc_config.xml").exists())
                self.assertEqual((backups / "global_prefs_override.xml").read_text(), original_prefs)
                self.assertFalse((backups / "global_prefs_override.xml.absent").exists())
                cc.write_text(setup.merge_xml(cc, "cc_config", {"ncpus": "1", "no_gpus": "1"}, "options"))
                prefs.write_text(setup.merge_xml(prefs, "global_preferences", setup.PREFS))
                setup.backup(cc, backups)
                setup.backup(prefs, backups)
                self.assertTrue((backups / "cc_config.xml.absent").exists())
                self.assertFalse((backups / "cc_config.xml").exists())
                self.assertEqual((backups / "global_prefs_override.xml").read_text(), original_prefs)

    def test_start_timeout_exceeds_unit_startup_deadline(self):
        self.assertGreater(setup.START_TIMEOUT, 600)
        completed = unittest.mock.Mock(returncode=0, stdout="ok\n", stderr="")
        with patch.object(setup.subprocess, "run", return_value=completed) as subrun:
            setup.run(["systemctl", "stop", "boinc-client.service"])
            setup.run(["systemctl", "start", "boinc-client.service"], timeout=setup.START_TIMEOUT)
            self.assertEqual(subrun.call_args_list[0].kwargs["timeout"], 180)
            self.assertEqual(subrun.call_args_list[1].kwargs["timeout"], setup.START_TIMEOUT)

    def test_duplicate_settings_and_wrong_xml_fail_without_writing(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prefs.xml"
            for xml in ["<other/>", "<global_preferences><run_if_user_active>0</run_if_user_active><run_if_user_active>1</run_if_user_active></global_preferences>"]:
                path.write_text(xml)
                with self.assertRaises(setup.SetupError):
                    setup.merge_xml(path, "global_preferences", setup.PREFS)
                self.assertEqual(path.read_text(), xml)
            cc = Path(tmp) / "cc_config.xml"
            dup_sections = "<cc_config><options><ncpus>1</ncpus></options><options><ncpus>8</ncpus></options></cc_config>"
            cc.write_text(dup_sections)
            with self.assertRaises(setup.SetupError):
                setup.merge_xml(cc, "cc_config", {"ncpus": "1", "no_gpus": "1"}, "options")
            self.assertEqual(cc.read_text(), dup_sections)

    def test_architecture_check_accepts_x86_64_and_aarch64_and_rejects_others(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp)
            with patch.object(
                setup,
                "resolve_launch",
                return_value=("/usr/bin/boinc", str(data), []),
            ), patch.object(
                setup,
                "run",
                side_effect=setup.SetupError("reached post-arch systemctl User query"),
            ):
                for arch in ("x86_64", "aarch64"):
                    fake_uname = unittest.mock.Mock(machine=arch)
                    with patch.object(setup.os, "uname", return_value=fake_uname):
                        with self.assertRaisesRegex(
                            setup.SetupError, "reached post-arch systemctl User query"
                        ):
                            setup.setup("boinc-client.service", data, IMAGE)
                fake_uname = unittest.mock.Mock(machine="armv7l")
                with patch.object(setup.os, "uname", return_value=fake_uname):
                    with self.assertRaisesRegex(
                        setup.SetupError, "supports Linux x86_64 and aarch64 only"
                    ):
                        setup.setup("boinc-client.service", data, IMAGE)


if __name__ == "__main__":
    unittest.main()
