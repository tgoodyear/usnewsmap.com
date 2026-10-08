"""Tests for scripts/qwcluster-report.py (python3 -m unittest scripts/test_qwcluster_report.py).

The fixtures in testdata/qwcluster/ are `qwcluster report` lines from a
local run of usnm-qwcluster load and bench on a two-node cluster, trimmed."""

import importlib.util
import json
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("report", os.path.join(HERE, "qwcluster-report.py"))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


def fixture(name):
    with open(os.path.join(HERE, "testdata", "qwcluster", name)) as f:
        return json.load(f)


PRICES = {"vcpu_active": 0.000024, "vcpu_idle": 0.000003, "gib_active": 0.000003,
          "gib_idle": 0.000003, "blob_gb_month": 0.0184}


class ReportTest(unittest.TestCase):
    def test_reads_query_rows(self):
        v = {"tables": [{"columns": [{"name": "Kind"}, {"name": "Report"}],
                         "rows": [["load", "{\"a\": 1}"], ["bench", "{cut short"]]}]}
        rows = report.rows_of(v)
        self.assertEqual(rows[0], {"Kind": "load", "Report": "{\"a\": 1}"})
        self.assertEqual(report.parse_reports(rows), [("load", {"a": 1})])

    def test_spans(self):
        self.assertEqual(report.span_of("2d"), "P2D")
        self.assertEqual(report.span_of("6h"), "PT6H")
        with self.assertRaises(SystemExit):
            report.span_of("2w")

    def test_the_sidecar_costs_what_238_says(self):
        # #238: one 2 vCPU / 4 GiB sidecar, all idle, $47.30 a month.
        idle = report.replica_cost(PRICES, 2, 4, 0, report.SECONDS_PER_MONTH)
        self.assertAlmostEqual(idle, 47.30, places=2)
        self.assertAlmostEqual(report.steady_state(PRICES, 3, 2), 3 * idle, places=6)
        # Active seconds cost more for vCPU only.
        self.assertGreater(report.steady_state(PRICES, 1, 2, 0.9), idle)

    def test_picks_list_prices(self):
        aca = [{"meterName": n, "retailPrice": v, "tierMinimumUnits": 0} for n, v in [
            ("Standard vCPU Active Usage", 2.4e-05), ("Standard vCPU Idle Usage", 3e-06),
            ("Standard Memory Active Usage", 3e-06), ("Standard Memory Idle Usage", 3e-06)]]
        blob = [{"meterName": "Hot LRS Data Stored", "retailPrice": 0.0184, "tierMinimumUnits": 0},
                {"meterName": "Hot LRS Data Stored", "retailPrice": 0.0177, "tierMinimumUnits": 51200}]
        self.assertEqual(report.pick_prices(aca, blob), PRICES)
        with self.assertRaises(SystemExit):
            report.pick_prices([], blob)

    def test_summarizes_metrics_per_minute(self):
        def series(name, agg, values):
            return {"name": {"value": name}, "timeseries": [{"data": [
                {"timeStamp": f"2026-10-09T00:0{i}:00Z", agg: x} for i, x in enumerate(values)]}]}
        v = {"value": [
            series("UsageNanoCores", "average", [5e6, 1.5e9, 2e6]),
            series("WorkingSetBytes", "maximum", [2**30, 2 * 2**30, 2**30]),
            series("RxBytes", "total", [6000, 600000, 120000]),
            series("TxBytes", "total", [6000, 600000, 0]),
        ]}
        m = report.summarize_metrics(v)
        self.assertEqual(m["minutes"], 3)
        self.assertAlmostEqual(m["cpu_avg"], (0.005 + 1.5 + 0.002) / 3)
        self.assertEqual(m["mem_max_mib"], 2048)
        # Minute 1 is busy; minute 3 is quiet on CPU but over 1,000 bytes/s.
        self.assertEqual(m["idle_minutes"], 1)

    def test_tables_from_real_reports(self):
        load = json.loads(fixture("load.json")["Report"])
        bench = json.loads(fixture("bench.json")["Report"])
        rows = report.load_rows([load])
        self.assertEqual(rows[0][0], "s2ix")
        self.assertEqual(rows[0][1], 2)
        self.assertIn("qw-0", rows[0][-1])
        b = report.bench_rows([bench])
        self.assertEqual([r[3] for r in b], ["first", "warm", "c1", "c2", "c4", "c10"])
        self.assertTrue(all(r[-1] == "yes" for r in b))
        md = report.table(["a", "b"], [[1, 2]])
        self.assertEqual(md.splitlines(), ["| a | b |", "|---|---|", "| 1 | 2 |"])
        self.assertGreater(report.secs_between(load["started_at"], load["sealed_at"]), 0)

    def test_reads_reports_from_a_log_stream(self):
        load = fixture("load.json")["Report"]
        line = json.dumps({"timestamp": "t", "level": "INFO",
                           "fields": {"message": "qwcluster report", "kind": "load", "report": load}})
        lines = [
            "noise",
            line,
            "2026-10-09T10:00:00Z " + json.dumps({"Log": line}),
            json.dumps({"fields": {"message": "cluster load progress"}}),
            json.dumps({"fields": {"message": "qwcluster report", "kind": "bench", "report": "{bad"}}),
        ]
        got = report.reports_from_lines(lines)
        self.assertEqual(len(got), 1)
        (kind, name), r = got[0]
        self.assertEqual((kind, name), ("load", "s2ix"))
        self.assertEqual(r["docs"], json.loads(load)["docs"])


if __name__ == "__main__":
    unittest.main()
