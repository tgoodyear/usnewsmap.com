import importlib.util
import json
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("host_eligibility", Path(__file__).with_name("host-eligibility.py"))
he = importlib.util.module_from_spec(spec)
spec.loader.exec_module(he)

SPEC = """<plan_classes>
  <plan_class><name>nvidia_vulkan_8b</name><gpu_type>nvidia</gpu_type><min_gpu_ram_mb>7000</min_gpu_ram_mb></plan_class>
  <plan_class><name>nvidia_vulkan_6gb_8b</name><gpu_type>nvidia</gpu_type><min_gpu_ram_mb>5500</min_gpu_ram_mb><max_gpu_ram_mb>6999</max_gpu_ram_mb></plan_class>
  <plan_class><name>amd_vulkan_8b</name><gpu_type>ati</gpu_type><min_gpu_ram_mb>7000</min_gpu_ram_mb></plan_class>
  <plan_class><name>cpu_only</name></plan_class>
</plan_classes>"""


def gpu(kind, ram):
    return json.dumps([{"type": kind, "ram": ram}])


HOSTS = [
    (10, "Microsoft Windows 11", gpu("NVIDIA", 8192), 100),       # the Windows test VM
    (11, "Microsoft Windows 11", gpu("ATI", 16384), 100),         # an AMD Windows volunteer
    (12, "Microsoft Windows 10", gpu("NVIDIA", 6144), 100),       # a 6 GB NVIDIA card
    (13, "Linux Ubuntu", gpu("NVIDIA", 12288), 100),
    (14, "Microsoft Windows 11", "bad", 100),                     # misc unreadable
    (15, "Darwin", "null", 100),
    (16, "Microsoft Windows 11", json.dumps([{"type": "NVIDIA"}]), 100),  # vendor, no memory
]


class EligibilityTests(unittest.TestCase):
    def run_it(self, versions):
        return he.eligibility(versions, HOSTS, he.plan_classes(SPEC), 7)["versions"]

    def ids(self, entry, key="candidates"):
        return [h["host"] for h in entry[key]]

    def test_windows_nvidia_8b_isolation_shows_the_test_vm_and_uncertain_hosts(self):
        (entry,) = self.run_it([("windows_x86_64", "nvidia_vulkan_8b", 121, 1)])
        self.assertEqual(self.ids(entry), [10])
        self.assertEqual(self.ids(entry, "uncertain"), [14, 16])
        self.assertTrue(entry["held"])

    def test_memory_bounds_and_vendor(self):
        six, amd = self.run_it([("windows_x86_64", "nvidia_vulkan_6gb_8b", 121, 0),
                                ("windows_x86_64", "amd_vulkan_8b", 121, 0)])
        self.assertEqual(self.ids(six), [12])
        self.assertEqual(self.ids(amd), [11])

    def test_os_family_and_cpu_classes(self):
        linux, cpu = self.run_it([("x86_64-pc-linux-gnu", "nvidia_vulkan_8b", 121, 0),
                                  ("x86_64-apple-darwin", "", 121, 0)])
        self.assertEqual(self.ids(linux), [13])
        self.assertEqual(self.ids(cpu), [15])

    def test_a_gpu_of_the_class_vendor_without_memory_is_uncertain_not_dropped(self):
        # Copilot on #2178: dropping it would falsely show isolation.
        six, amd = self.run_it([("windows_x86_64", "nvidia_vulkan_6gb_8b", 1, 0),
                                ("windows_x86_64", "amd_vulkan_8b", 1, 0)])
        self.assertIn(16, self.ids(six, "uncertain"))
        self.assertNotIn(16, self.ids(amd, "uncertain"))
        self.assertNotIn(16, self.ids(amd))

    def test_an_unknown_or_non_string_vendor_is_uncertain_not_dropped(self):
        # Copilot on #2178 (round 2): {"type": "GPU", "vendor": "nvidia"} left the host out of both lists.
        cases = [{"type": "GPU", "vendor": "nvidia", "ram_mb": 8192}, {"type": 7, "ram_mb": 8192},
                 {"vendor": ["nvidia"], "ram_mb": 8192}, {"type": "nvidia", "vendor": "amd", "ram_mb": 8192},
                 {"type": "Quadro", "ram_mb": 8192}, {"ram_mb": 8192},
                 {"type": "nvidia", "ram_mb": True}, {"type": "nvidia", "ram_mb": "8192"},
                 {"type": "nvidia", "ram_mb": -1}]
        for gpu in cases:
            with self.subTest(gpu=gpu):
                self.assertIsNone(he.gpus(json.dumps([gpu])))
                hosts = [(20, "Microsoft Windows 11", json.dumps([gpu]), 100)]
                (entry,) = he.eligibility([("windows_x86_64", "nvidia_vulkan_8b", 1, 0)], hosts,
                                          he.plan_classes(SPEC), 7)["versions"]
                self.assertEqual([h["host"] for h in entry["uncertain"]], [20])
                self.assertEqual(entry["candidates"], [])
        self.assertEqual(he.gpus('[{"type": "NVIDIA", "vendor": "nvidia", "ram_mb": 8192}]')[0]["vendor"], "nvidia")

    def test_unknown_plan_class_is_flagged(self):
        (entry,) = self.run_it([("windows_x86_64", "no_such_class", 1, 0)])
        self.assertIn("error", entry)

    def test_gpu_parsing(self):
        self.assertEqual(he.gpus("null"), [])
        self.assertIsNone(he.gpus("bad"))
        self.assertIsNone(he.gpus('[{"ram": 1}]'))
        self.assertEqual(he.gpus('[{"type": "NVIDIA", "ram": 8589934592}]')[0]["ram_mb"], 8192)

    def test_argument_checks(self):
        self.assertEqual(he.main(["--mysql-defaults-file", "x", "--database", "db;drop",
                                  "--plan-class-spec", "x"]), 2)
        self.assertEqual(he.main(["--mysql-defaults-file", "x", "--database", "db",
                                  "--plan-class-spec", "x", "--days", "31"]), 2)


if __name__ == "__main__":
    unittest.main()
