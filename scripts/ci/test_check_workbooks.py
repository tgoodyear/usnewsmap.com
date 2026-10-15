"""scripts/ci/check-workbooks.py: the workbook definitions the Bicep module loads."""

import importlib.util
import json
import tempfile
import unittest
import unittest.mock
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check", HERE / "check-workbooks.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

P = "__WORKSPACE_ID__"
GOOD = {"version": "Notebook/1.0", "items": [{"type": 3, "content": {"crossComponentResources": [P]}}]}
MODULE = f"""var placeholder = '{P}'
resource a 'x' = {{ properties: {{ serializedData: replace(loadTextContent('a.json'), placeholder, ws) }} }}
"""


class Check(unittest.TestCase):
    def test_ocr_nodes_status_checks_counter_freshness_after_heartbeat(self):
        doc = json.loads((check.ROOT / "infra/workbooks/ocr-nodes.json").read_text())
        query = next(i["content"]["query"] for i in doc["items"] if i["name"] == "nodes")
        counter_check = 'isnull(LastCounters) or now() - LastCounters > 15m, "no counters"'
        health_query = (check.ROOT / "ops/queries/ocr-node-health.kql").read_text()
        self.assertIn(counter_check, health_query)
        self.assertIn(
            '| extend Status = case(\n    MinutesSince > 5, "silent",\n'
            f'    {counter_check},\n    "ok")',
            query,
        )
        self.assertIn("| join kind=leftouter recent on Node", query)
        self.assertIn("by Node, LastCounters;", query)

    def test_ocr_worker_chart_keeps_sources_in_separate_series(self):
        doc = json.loads((check.ROOT / "infra/workbooks/ocr-nodes.json").read_text())
        query = next(i["content"]["query"] for i in doc["items"] if i["name"] == "worker-chart")
        for source in ("syslog", "traces", "exceptions"):
            self.assertIn(f'Source = "{source}"', query)
        self.assertIn(
            '| summarize Lines = count() by bin(TimeGenerated, {TimeRange:grain}), '
            'Series = strcat(Node, " ", Source, " ", Level)',
            query,
        )

    def problems(self, doc):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "a.json"
            path.write_text(doc if isinstance(doc, str) else json.dumps(doc))
            return check.problems(path, P)

    def test_the_committed_workbooks_pass(self):
        placeholder, files = check.module_parts(check.MODULE.read_text())
        self.assertEqual(placeholder, P)
        self.assertEqual(sorted(files), ["../workbooks/api.json", "../workbooks/ocr-nodes.json", "../workbooks/pipeline.json"])
        with unittest.mock.patch.object(check.sys, "argv", ["check"]):
            self.assertEqual(check.main(), 0)

    def test_a_good_workbook_passes(self):
        self.assertEqual(self.problems(GOOD), [])

    def test_broken_workbooks(self):
        rid = "/subscriptions/0000/resourceGroups/rg/providers/Microsoft.OperationalInsights/workspaces/w"
        for why, doc, want in [
            ("not JSON", '{"version": ', "not readable JSON"),
            ("an array", "[]", "not a JSON object"),
            ("no version", {"items": GOOD["items"]}, "no `version` string"),
            ("no items", {"version": "Notebook/1.0", "x": P}, "no `items` array, or an empty one"),
            ("empty items", {"version": "Notebook/1.0", "items": [], "x": P}, "no `items` array, or an empty one"),
            ("an item not an object", {**GOOD, "items": [1, {"x": P}]}, "an item that isn't an object"),
            ("no placeholder", {"version": "Notebook/1.0", "items": [{}]}, f"never names the workspace placeholder {P}"),
            ("a real resource id", {**GOOD, "x": rid}, f"holds a resource id: put {P} back in its place"),
        ]:
            got = self.problems(doc)
            self.assertEqual(len(got), 1, (why, got))
            self.assertTrue(got[0].startswith(want), (why, got))

    def test_a_module_the_check_cant_read_fails(self):
        with tempfile.TemporaryDirectory() as d:
            module = Path(d) / "workbooks.bicep"
            module.write_text("param x string\n")
            with unittest.mock.patch.object(check.sys, "argv", ["check", str(module)]):
                self.assertEqual(check.main(), 1)
            module.write_text(MODULE)
            Path(d, "a.json").write_text(json.dumps(GOOD))
            with unittest.mock.patch.object(check.sys, "argv", ["check", str(module)]):
                self.assertEqual(check.main(), 0)
            Path(d, "a.json").unlink()
            with unittest.mock.patch.object(check.sys, "argv", ["check", str(module)]):
                self.assertEqual(check.main(), 1)


if __name__ == "__main__":
    unittest.main()
