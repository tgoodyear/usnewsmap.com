"""Regression checks for the server's daemon configuration."""

from pathlib import Path
import math
import os
import shlex
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET


class DaemonConfig(unittest.TestCase):
    def test_intel_pilot_has_distinct_eligibility_and_assigned_dispatch(self):
        root = Path(__file__).parent
        config = ET.parse(root / "config.xml.template")
        self.assertEqual(config.findtext("./config/enable_assignment"), "1")
        self.assertIsNone(config.find("./config/enable_assigned_jobs"))
        plans = ET.parse(root / "plan_class_spec.xml")
        classes = [p for p in plans.findall("plan_class") if p.findtext("name") == "docker_intel_opencl_4b"]
        self.assertEqual(len(classes), 1)
        plan = classes[0]
        self.assertEqual(plan.findtext("gpu_type"), "intel")
        self.assertIsNotNone(plan.find("docker"))
        self.assertIsNotNone(plan.find("opencl"))
        self.assertIsNone(plan.find("cuda"))
        self.assertEqual(plan.findtext("min_batch"), "104")
        self.assertEqual(plan.findtext("max_batch"), "104")
        self.assertEqual(plan.findtext("mem_usage_base_mb"), "9663.676416")
        surya_intel = [p for p in plans.findall("plan_class") if p.findtext("name") == "docker_intel_vulkan"]
        self.assertEqual(len(surya_intel), 1)
        surya_plan = surya_intel[0]
        self.assertEqual(surya_plan.findtext("gpu_type"), "intel")
        self.assertIsNotNone(surya_plan.find("docker"))
        self.assertIsNotNone(surya_plan.find("opencl"))
        self.assertIsNone(surya_plan.find("cuda"))
        self.assertEqual(surya_plan.findtext("min_gpu_ram_mb"), "4096")
        self.assertEqual(surya_plan.findtext("gpu_ram_used_mb"), "4096")
        self.assertEqual(surya_plan.findtext("min_wu_id"), "2")
        self.assertEqual(surya_plan.findtext("max_wu_id"), "1")

    def test_feeder_interleaves_all_apps_with_expanded_shmem(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [shlex.split(cmd.text) for cmd in root.findall("./daemons/daemon/cmd")]
        self.assertEqual(
            [cmd for cmd in commands if cmd[0] == "feeder"],
            [["feeder", "-d", "2", "--allapps", "--purge_stale", "5"]],
        )
        self.assertEqual(root.findtext("./config/shmem_work_items"), "1000")

    def test_feeder_query_covers_weighted_cache_and_qwen_backlog_with_headroom(self):
        root = Path(__file__).parent
        config = ET.parse(root / "config.xml.template")
        query_size = int(config.findtext("./config/feeder_query_size", "0"))
        cache_size = int(config.findtext("./config/shmem_work_items"))
        apps = ET.parse(root / "project.xml").findall("app")
        weights = {app.findtext("name"): float(app.findtext("weight")) for app in apps}
        total_weight = sum(weights.values())
        self.assertGreater(total_weight, 0)
        budgets = {}
        for name, weight in weights.items():
            self.assertGreaterEqual(weight, 0)
            # Pinned feeder.cpp: floor(0.5 + enum_limit * weight / app_weight_sum).
            budgets[name] = math.floor(0.5 + query_size * weight / total_weight)
            if weight > 0:
                with self.subTest(app=name):
                    self.assertGreaterEqual(budgets[name], 2 * cache_size)
        # Measured 28 October sizing baseline: all unsent Qwen replicas, including blocked ones.
        qwen_ready_results = 635
        self.assertGreaterEqual(budgets["usnm_qwen"], 4 * qwen_ready_results)

        # Pinned feeder.cpp weighted_interleave(): purge_stale rotation alone cannot expose
        # a backlog larger than the app's static cache share to an eligible host request.
        names = list(weights)
        credit = [1e-100 if weights[name] == 0 else 0.0 for name in names]
        slots = dict.fromkeys(names, 0)
        for _ in range(cache_size):
            best = max(range(len(names)), key=lambda i: (credit[i], -i))
            # C divides by zero to infinity: a zero-weight app takes one slot, then no more.
            credit[best] -= 1 / weights[names[best]] if weights[names[best]] else math.inf
            slots[names[best]] += 1
        self.assertGreaterEqual(slots["usnm_qwen"], qwen_ready_results)
        for name, weight in weights.items():
            if weight > 0 and name != "usnm_qwen":
                with self.subTest(app=name):
                    self.assertGreaterEqual(slots[name], 40)

    def test_replicas_need_different_hosts_not_different_accounts(self):
        # The project's hosts share one account; a per-account rule starves them on quorum-2 work.
        config = ET.parse(Path(__file__).with_name("config.xml.template")).find("./config")
        self.assertIsNotNone(config.find("one_result_per_host_per_wu"))
        self.assertIsNone(config.find("one_result_per_user_per_wu"))

    def test_matching_runtime_links_are_not_removed_or_replaced(self):
        root = Path(__file__).resolve().parents[2]
        source = (root / "contrib-compute/images/server/lib.sh").read_text()
        links = source.split('  if [ "$(readlink "$PROJ/config.xml"', 1)[1]
        links = 'if [ "$(readlink "$PROJ/config.xml"' + links.split("\n}", 1)[0]
        with tempfile.TemporaryDirectory(dir=root) as directory:
            work = Path(directory)
            project = work / "project"
            secrets = work / "secrets"
            project.mkdir()
            secrets.mkdir()
            (secrets / "keys").mkdir()
            (secrets / "keys/upload_public").write_text("public fixture")
            (secrets / "config.xml").write_text("<config/>")
            (project / "keys").symlink_to(secrets / "keys")
            (project / "config.xml").symlink_to(secrets / "config.xml")
            before = [(project / name).lstat().st_ino for name in ("keys", "config.xml")]
            subprocess.run(
                ["sh", "-eu", "-c", links], check=True,
                env={**os.environ, "PROJ": str(project), "SECRETS": str(secrets)},
            )
            self.assertEqual(before, [(project / name).lstat().st_ino for name in ("keys", "config.xml")])
            self.assertEqual((secrets / "keys/upload_public").read_text(), "public fixture")

    def test_db_purge_runs_continuously_with_thirty_day_retention(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [
            shlex.split(cmd.text)
            for cmd in root.findall("./daemons/daemon/cmd")
        ]
        purge = [cmd for cmd in commands if cmd[0] == "db_purge"]
        self.assertEqual(purge, [["db_purge", "-d", "2", "--min_age_days", "30"]])

    def test_revision69_settings_preserve_native_eligibility_and_closed_gpu_admission(self):
        root = Path(__file__).parent
        config = ET.parse(root / "config.xml.template")
        self.assertEqual(config.findtext("./config/next_rpc_delay"), "3600")
        plans = {p.findtext("name"): p for p in ET.parse(root / "plan_class_spec.xml").findall("plan_class")}
        for name in ("mt", "apple_gpu_8b"):
            self.assertEqual(plans[name].findtext("min_core_client_version"), "80211")
        for name in ("docker_intel_vulkan", "intel_vulkan_8b", "nvidia_vulkan_8b", "amd_vulkan_8b", "nvidia_cuda_8b", "nvidia_cuda_2b"):
            with self.subTest(plan=name):
                self.assertEqual(plans[name].findtext("min_wu_id"), "2")
                self.assertEqual(plans[name].findtext("max_wu_id"), "1")
        amd = plans["amd_vulkan_8b"]
        for tag, value in (("gpu_type", "ati"), ("ngpus", "1"), ("min_batch", "8"),
                           ("max_batch", "8"), ("min_gpu_ram_mb", "7000"),
                           ("gpu_ram_used_mb", "6500"), ("avg_ncpus", "2")):
            self.assertEqual(amd.findtext(tag), value)
        self.assertIsNotNone(amd.find("opencl"))
        self.assertIsNone(amd.find("cuda"))
        self.assertIsNone(amd.find("docker"))

    def test_high_throughput_scheduler_dispatch_limits(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        self.assertEqual(root.findtext("./config/min_sendwork_interval"), "2")
        self.assertEqual(root.findtext("./config/next_rpc_delay"), "3600")
        self.assertEqual(root.findtext("./config/max_wus_in_progress"), "32")
        self.assertEqual(root.findtext("./config/daily_result_quota"), "50000")
        self.assertEqual(root.findtext("./config/max_wus_to_send"), "64")
