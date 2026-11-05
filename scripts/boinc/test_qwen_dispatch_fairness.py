import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import sys
import types
import unittest
from unittest import mock
import uuid

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("dispatch_fairness", ROOT / "scripts/boinc/qwen-dispatch-fairness.py")
F = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(F)

SHMEM = """apps:
id: 6 name: usnm_qwen hr: 0 weight: 16.00 beta: 0 hav: 0 nci: 0
ready: 1
max_wu_results: 5
slot          app      WU ID  result ID      batch     kwds   priority in shmem   size class need reliable inf count   version
   0: ---
   1    usnm_qwen        101       1001          8                   0      51s        -1           no         0         0
   2    usnm_qwen        103       1003          8  a b 10s          0     302s        -1           no         0         0
   3: PID 77: result 1004
   4 usnm_quality      900       9000          0                   0     150s        -1           no         0         0
"""


def snapshot():
    # Units 101-105 in feeder order after 106, which a calibration priority raises to the head.
    return {
        "snapshot": 1793880000, "since": 1793145600, "recent_hours": 24,
        "bounds": {"amd_vulkan_8b": None, "intel_vulkan_8b": [2, 1], "nvidia_vulkan_8b": [104, 106]},
        "hosts": {"5": "amd_vulkan_8b", "9": "intel_vulkan_8b", "10": "nvidia_vulkan_8b"},
        "daily": [[9, 20761, 5, 2, 1, 0, 1, 0, 1, 0], [5, 20761, 7, 3, 0, 1, 0, 2, 0, 1]],
        "unsent": [[1006, 106, 10], [1001, 101, 0], [1002, 102, 0], [1003, 103, 0], [1004, 104, 0], [1005, 105, 0], [1015, 105, 0]],
        "taken": [[106, 5], [106, 10], [101, 5], [102, 5], [103, 5], [104, 5], [104, 9]],
        "recent": [[5, 101, 0], [5, 106, 10], [10, 104, 0], [9, 99, 0]],
        "cache": {"slots": 5, "empty": 1, "reserved": 1, "apps": {}, "results": [[1001, 51], [1003, 302], [1006, 5]]},
    }


class Shmem(unittest.TestCase):
    def test_reads_slots_around_free_text_keywords(self):
        cache = F.parse_shmem(SHMEM)
        self.assertEqual((cache["slots"], cache["empty"], cache["reserved"]), (5, 1, 1))
        self.assertEqual(cache["apps"], {"usnm_qwen": 2, "usnm_quality": 1})
        self.assertEqual(cache["results"], [[1001, 51], [1003, 302]])


class Report(unittest.TestCase):
    def test_blocked_head_takers_and_cache(self):
        a = F.analyse(snapshot())
        # 106: AMD and NVIDIA hold it and Intel is closed; 101-103: AMD holds them and NVIDIA's bounds exclude them.
        self.assertEqual((a["units"], a["replicas"], a["blocked"], a["head"]), (6, 7, 4, 4))
        self.assertEqual(a["can take"], {5: 1, 10: 2})
        self.assertEqual(a["only taker"], {10: 1})
        self.assertEqual(a["free of blocked"], {9: 4, 10: 3})
        self.assertEqual(a["past head"], {10: 1})
        self.assertEqual(a["cache"], {"qwen slots": 3, "empty": 1, "reserved": 1, "slots": 5, "unsent cached": 3,
                                      "blocked cached": 3, "oldest": 302})

    def test_tables(self):
        d = ROOT / (".local-dispatch-fairness-tests-" + uuid.uuid4().hex)
        d.mkdir()
        self.addCleanup(shutil.rmtree, d)
        (d / "snapshot.json").write_text(json.dumps(snapshot()))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            F.main(["report", "--snapshot", str(d / "snapshot.json")])
        text = out.getvalue()
        self.assertIn("| 9 | intel_vulkan_8b | 2026-11-04 | 5 | 2 | 1 | 0 | 1 | 0 | 1 | 0 |", text)
        self.assertIn("| 9 | intel_vulkan_8b | closed | 0 | 0 | 4 | 1 | 0 |", text)
        self.assertIn("| 10 | nvidia_vulkan_8b | 104–106 | 2 | 1 | 3 | 1 | 1 |", text)
        self.assertIn("| 5 | amd_vulkan_8b | unbounded | 1 | 0 | 0 | 2 | 0 |", text)
        self.assertIn("4 units no listed host can take now; the first 4 units in feeder order", text)


class FakeCursor:
    def __init__(self, log, sizes):
        self.log, self.sizes, self.rows = log, sizes, []

    def execute(self, sql, params=None):
        self.log.append((sql, params))
        if "UTC_TIMESTAMP" in sql:
            self.rows = [(1793880000,)]
        elif "FROM app " in sql:
            self.rows = [(6,)]
        elif "av.plan_class" in sql:
            self.rows = [(9, "intel_vulkan_8b", 5), (9, "nvidia_vulkan_8b", 3)][:self.sizes.get("hosts", 2)]
        elif "server_state=2 ORDER BY" in sql:
            self.rows = [(1001, 101, 0)] * self.sizes.get("unsent", 1)
        else:
            self.rows = []

    def fetchone(self):
        return self.rows[0]

    def fetchall(self):
        return self.rows


class Collect(unittest.TestCase):
    def setUp(self):
        self.dir = ROOT / (".local-dispatch-fairness-tests-" + uuid.uuid4().hex)
        self.dir.mkdir()
        self.addCleanup(shutil.rmtree, self.dir)
        (self.dir / "config.xml").write_text(
            "<boinc><config><db_host>h</db_host><db_user>u</db_user><db_passwd>p</db_passwd><db_name>n</db_name></config></boinc>")
        (self.dir / "spec.xml").write_text(
            "<plan_classes><plan_class><name>intel_vulkan_8b</name><min_wu_id>2</min_wu_id><max_wu_id>1</max_wu_id></plan_class>"
            "<plan_class><name>amd_vulkan_8b</name></plan_class></plan_classes>")
        self.log, self.closed = [], []

    def run_collect(self, sizes, limit="10"):
        log, closed = self.log, self.closed

        class Conn:
            def cursor(self):
                return FakeCursor(log, sizes)

            def rollback(self):
                log.append(("ROLLBACK", None))

            def close(self):
                closed.append(True)

        fake = types.ModuleType("MySQLdb")
        fake.connect = lambda **kw: Conn()
        out = io.StringIO()
        with mock.patch.dict(sys.modules, {"MySQLdb": fake}), contextlib.redirect_stdout(out):
            F.main(["collect", "--since", "2026-10-28T00:00:00Z", "--config", str(self.dir / "config.xml"),
                    "--plan-class-spec", str(self.dir / "spec.xml"), "--limit", limit])
        return json.loads(out.getvalue())

    def test_reads_in_one_read_only_transaction(self):
        snap = self.run_collect({})
        sql = [s for s, _ in self.log]
        self.assertLess(sql.index("START TRANSACTION READ ONLY"), min(i for i, s in enumerate(sql) if "FROM result" in s))
        self.assertEqual(sql[-1], "ROLLBACK")
        self.assertEqual(self.closed, [True])
        self.assertTrue(all(s.endswith(" LIMIT %s") for s in sql if "FROM result" in s))
        self.assertFalse([s for s in sql if s.split()[0].upper() in ("INSERT", "UPDATE", "DELETE", "REPLACE")])
        # The host's latest send names its class.
        self.assertEqual(snap["hosts"], {"9": "intel_vulkan_8b"})
        self.assertEqual(snap["bounds"], {"intel_vulkan_8b": [2, 1]})
        self.assertEqual(snap["unsent"], [[1001, 101, 0]])

    def test_refuses_at_the_limit_and_still_rolls_back(self):
        with self.assertRaises(SystemExit):
            self.run_collect({"hosts": 1, "unsent": 2}, limit="2")
        self.assertEqual([s for s, _ in self.log].count("ROLLBACK"), 1)
        self.assertEqual(self.closed, [True])


if __name__ == "__main__":
    unittest.main()
