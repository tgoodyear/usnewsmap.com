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
    def test_line_class_admits_the_docker_hosts_for_the_line_batch_only(self):
        root = Path(__file__).parent
        plans = {p.findtext("name"): p for p in ET.parse(root / "plan_class_spec.xml").findall("plan_class")}
        lines, docker = plans["docker_lines"], plans["docker"]
        # Host gates equal docker's, so no host takes one class without the other (#1621): a host
        # left without the pinned version would cache "no version" for the whole app for that
        # request (sched_version.cpp, get_app_version) and skip its docker work too.
        gates = lambda p: sorted((c.tag, (c.text or "").strip()) for c in p
                                 if c.tag not in ("name", "min_batch", "max_batch"))
        self.assertEqual(gates(lines), gates(docker))
        self.assertIsNone(docker.find("min_batch"))
        self.assertEqual((lines.findtext("min_batch"), lines.findtext("max_batch")), ("1621", "1621"))
        # No other class serves that batch, and usnm-boinc puts line jobs in it.
        for name, plan in plans.items():
            if name != "docker_lines" and plan.find("min_batch") is not None:
                self.assertFalse(int(plan.findtext("min_batch")) <= 1621 <= int(plan.findtext("max_batch")), name)
        submit = (root.parents[1] / "crates/usnm-boinc/src/submit.rs").read_text()
        self.assertIn("pub const LINE_BATCH: u32 = 1621;", submit)

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
        self.assertEqual(root.findtext("./config/shmem_work_items"), "4096")

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
        # A zero weight gives an app one cache slot and no query budget, so the feeder never
        # caches its work. Only an app whose units are all assigned to a host may have it:
        # the scheduler sends assigned work from the database (BOINC 22bbebe, sched_assign.cpp).
        self.assertEqual(sorted(name for name, weight in weights.items() if weight == 0), ["usnm_gold_native"])
        # 30 October: 1788-1798 unsent replicas exceeded the former 666-slot Qwen share.
        qwen_ready_results = 1800
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
                    self.assertGreaterEqual(slots[name], 160)

    def test_replicas_need_different_hosts_not_different_accounts(self):
        # The project's hosts share one account; a per-account rule starves them on quorum-2 work.
        config = ET.parse(Path(__file__).with_name("config.xml.template")).find("./config")
        self.assertIsNotNone(config.find("one_result_per_host_per_wu"))
        self.assertIsNone(config.find("one_result_per_user_per_wu"))

    def test_matching_runtime_links_are_not_removed_or_replaced(self):
        root = Path(__file__).resolve().parents[2]
        source = (root / "boinc/images/server/lib.sh").read_text()
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

    def test_retired_apps_get_no_daemons_or_validator_links(self):
        # #1449: the apps retired in "Retiring an app" (docs/boinc/server/server-and-apps.md) leave the
        # status page; every other app keeps one validator and one assimilator.
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(dir=root) as directory:
            work = Path(directory)
            for name in ("path", "secrets", "share/usnmocr/bin"):
                (work / name).mkdir(parents=True)
            (work / "path/usnm-boinc").write_text("#!/bin/sh\n")
            (work / "path/usnm-boinc").chmod(0o755)
            (work / "secrets/db-runtime").write_text("fixture")
            subprocess.run(
                ["bash", "-eu", "-c", '. "$1"; SECRETS=$2 TEMPLATES=$3; chown() { :; }; render_config', "_",
                 str(root / "boinc/images/server/lib.sh"), str(work / "secrets"), str(Path(__file__).parent)],
                check=True,
                env={**os.environ, "PATH": f"{work / 'path'}{os.pathsep}{os.environ['PATH']}",
                     "KEY_VAULT_NAME": "kv", "DB_HOST": "db", "DB_NAME": "usnmocr", "MASTER_HOST": "boinc.example",
                     "PROJECT_DIR": str(work / "share"), "PROJECT_HOST_OVERRIDE": "server"},
            )
            commands = [shlex.split(cmd.text) for cmd in
                        ET.parse(work / "secrets/config.xml").findall("./daemons/daemon/cmd")]
            daemons = sorted((cmd[0], cmd[cmd.index("--app") + 1]) for cmd in commands if "--app" in cmd)
            links = sorted(path.name for path in (work / "share/usnmocr/bin").iterdir())
        live = ("usnm_gold", "usnm_gold_native", "usnm_ndlocr", "usnm_quality", "usnm_qwen")
        self.assertEqual(daemons, sorted([("sample_trivial_validator", "usnm_ocr"),
                                          ("batch_collect_assimilator", "usnm_ocr"),
                                          *(("script_validator", app) for app in live),
                                          *(("batch_collect_assimilator", app) for app in live)]))
        self.assertEqual(links, ["usnm_gold_check", "usnm_ndlocr_check", "usnm_ndlocr_compare",
                                 "usnm_quality_check", "usnm_quality_compare", "usnm_qwen_check",
                                 "usnm_qwen_compare"])

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
        for name in ("mt", "apple_gpu_8b", "nvidia_cuda_2b", "nvidia_cuda_8b",
                     "intel_vulkan_8b", "nvidia_vulkan_8b", "amd_vulkan_8b"):
            self.assertEqual(plans[name].findtext("min_core_client_version"), "80211")
        for name in ("nvidia_vulkan_6gb_8b", "nvidia_vulkan_2b"):
            self.assertEqual(plans[name].findtext("min_core_client_version"), "80209")
        for name in ("docker_intel_vulkan", "intel_vulkan_8b", "nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b",
                     "nvidia_vulkan_2b", "amd_vulkan_8b", "nvidia_cuda_8b", "nvidia_cuda_2b"):
            with self.subTest(plan=name):
                self.assertEqual(plans[name].findtext("min_wu_id"), "2")
                self.assertEqual(plans[name].findtext("max_wu_id"), "1")
        amd = plans["amd_vulkan_8b"]
        for tag, value in (("gpu_type", "ati"), ("ngpus", "1"), ("min_batch", "8"),
                           ("max_batch", "8"), ("min_gpu_ram_mb", "7000"),
                           ("gpu_ram_used_mb", "6500"), ("avg_ncpus", "2"),
                           # An iGPU's shared RAM passes the RAM floor; peak FLOPS keeps it out.
                           ("min_gpu_peak_flops", "1.5e12")):
            self.assertEqual(amd.findtext(tag), value)
        self.assertIsNotNone(amd.find("opencl"))
        self.assertIsNone(amd.find("cuda"))
        self.assertIsNone(amd.find("docker"))
        # #1234: a 6 GB NVIDIA card (a GTX 1660 SUPER reports about 5750 MB) is admitted only
        # to batch-2 work, which the app reads with the 2B model fully offloaded.
        small = plans["nvidia_vulkan_2b"]
        for tag, value in (("gpu_type", "nvidia"), ("ngpus", "1"), ("min_batch", "2"),
                           ("max_batch", "2"), ("min_gpu_ram_mb", "4500"),
                           ("gpu_ram_used_mb", "4096"), ("avg_ncpus", "2")):
            self.assertEqual(small.findtext(tag), value)
        self.assertIsNotNone(small.find("cuda"))
        self.assertIsNone(small.find("docker"))
        # #1234: partial-offload 8B for 6 GB NVIDIA cards, about 13x slower than full offload,
        # so it stops below the full 8B class's floor and never reaches a card that can run it.
        six = plans["nvidia_vulkan_6gb_8b"]
        full = plans["nvidia_vulkan_8b"]
        for tag, value in (("gpu_type", "nvidia"), ("ngpus", "1"), ("min_batch", "8"),
                           ("max_batch", "8"), ("min_gpu_ram_mb", "5500"),
                           ("max_gpu_ram_mb", "6999"), ("gpu_ram_used_mb", "5500"),
                           ("avg_ncpus", "2")):
            self.assertEqual(six.findtext(tag), value)
        self.assertIsNotNone(six.find("cuda"))
        self.assertIsNone(six.find("docker"))
        self.assertEqual(int(six.findtext("max_gpu_ram_mb")) + 1, int(full.findtext("min_gpu_ram_mb")))
        self.assertIsNone(full.find("max_gpu_ram_mb"))
        # Only the 6 GB class carries a cap, which the patched scheduler enforces.
        capped = [name for name, plan in plans.items() if plan.find("max_gpu_ram_mb") is not None]
        self.assertEqual(capped, ["nvidia_vulkan_6gb_8b"])
        patch = (root.parent / "scripts/patch-server-gpu-ram-cap.py").read_text()
        self.assertIn('parse_double("max_gpu_ram_mb", max_gpu_ram_mb)', patch)
        base = (root.parent / "images/base/Containerfile").read_text()
        self.assertIn("patch-server-gpu-ram-cap.py --source /usr/local/boinc/sched/plan_class_spec.cpp", base)

    def test_high_throughput_scheduler_dispatch_limits(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        self.assertEqual(root.findtext("./config/min_sendwork_interval"), "2")
        self.assertEqual(root.findtext("./config/next_rpc_delay"), "3600")
        self.assertEqual(root.findtext("./config/max_wus_in_progress"), "32")
        self.assertEqual(root.findtext("./config/daily_result_quota"), "50000")
        self.assertEqual(root.findtext("./config/max_wus_to_send"), "64")

    def test_qwen_tasks_in_progress_are_capped_per_gpu(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        apps = root.findall("./config/max_jobs_in_progress/app")
        self.assertEqual([a.findtext("app_name") for a in apps], ["usnm_qwen"])
        limit = apps[0].find("gpu_limit")
        self.assertEqual(limit.findtext("jobs"), "6")
        self.assertIsNotNone(limit.find("per_proc"))
        self.assertIsNone(apps[0].find("cpu_limit"))

    def test_project_down_replies_ask_clients_back_in_twenty_minutes(self):
        # #1462: without it, BOINC's default (sched_config.cpp L93 at 22bbebe0) tells every client
        # that asks during a feeder restart to wait an hour.
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        self.assertEqual(root.findtext("./config/maintenance_delay"), "1200")


class NoScanDeletion(unittest.TestCase):
    """Scans in download/ are never deleted (2026-11-08)."""

    def test_file_deleter_never_removes_input_files(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [shlex.split(cmd.text) for cmd in root.findall("./daemons/daemon/cmd")]
        deleters = [cmd for cmd in commands if cmd[0] == "file_deleter"]
        self.assertEqual(deleters, [["file_deleter", "-d", "2", "--preserve_wu_files"]])
        for task in root.findall("./tasks/task"):
            if task.findtext("cmd", "").split()[0] == "antique_file_deleter":
                self.assertEqual(task.findtext("disabled"), "1")

    def test_every_input_template_marks_every_file_no_delete(self):
        templates = sorted((Path(__file__).parents[1] / "apps").glob("*_in"))
        self.assertGreater(len(templates), 10)
        for path in templates:
            infos = ET.parse(path).findall("./file_info")
            self.assertTrue(infos, path.name)
            for info in infos:
                with self.subTest(template=path.name, number=info.findtext("number")):
                    self.assertIsNotNone(info.find("no_delete"))
