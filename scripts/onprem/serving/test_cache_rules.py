"""Rule-level Cloudflare updates preserve other services and fail on drift."""

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("cache_rules", HERE / "apply-cache-rules.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
DESIRED = json.loads((HERE / "cache-rules.json").read_text())


class CacheRules(unittest.TestCase):
    def request(self, method, path, body=None):
        self.calls.append((method, path, body))
        if method == "GET":
            result = copy.deepcopy(self.state)
            if self.drift and len(self.calls) > 1:
                result["rules"][0]["expression"] = "false"
            return result
        rule = {k: v for k, v in body.items() if k != "position"}
        if method == "PATCH":
            old = next(r for r in self.state["rules"] if r["id"] == path.split("/")[-1])
            self.state["rules"].remove(old)
            rule["id"] = old["id"]
        else:
            rule["id"] = f"new-{len(self.calls)}"
        self.state["rules"].append(rule)
        return copy.deepcopy(self.state)

    def setUp(self):
        self.calls = []
        self.drift = False
        self.state = {"id": "ruleset", "rules": [
            {"id": "ocr", "ref": "ocr", "expression": 'http.host eq "ocr.usnewsmap.com"'},
            json.loads((HERE / "basemap-cache-rule.json").read_text()) | {"id": "basemap"},
        ]}

    def test_create_and_update_preserve_other_services_and_bypass_last(self):
        original = copy.deepcopy(self.state["rules"])
        MODULE.apply(self.request, "zone", DESIRED)
        self.assertEqual(self.state["rules"][:2], original)
        self.assertEqual([r["ref"] for r in self.state["rules"][2:]],
                         [r["ref"] for r in DESIRED])
        self.assertEqual(sum(m == "POST" for m, _, _ in self.calls), 3)
        self.calls.clear()
        MODULE.apply(self.request, "zone", DESIRED)
        self.assertEqual(len(self.state["rules"]), 5)
        self.assertEqual(sum(m == "PATCH" for m, _, _ in self.calls), 3)
        self.assertFalse(any(m in ("PUT", "DELETE") for m, _, _ in self.calls))

    def test_readback_detects_unrelated_rule_drift(self):
        self.drift = True
        with self.assertRaisesRegex(ValueError, "unrelated cache rules changed"):
            MODULE.apply(self.request, "zone", DESIRED)

    def test_duplicate_managed_refs_abort_before_writing(self):
        self.state["rules"] += [DESIRED[0] | {"id": "a"}, DESIRED[0] | {"id": "b"}]
        with self.assertRaisesRegex(ValueError, "duplicate managed ref"):
            MODULE.apply(self.request, "zone", DESIRED)
        self.assertEqual([m for m, _, _ in self.calls], ["GET"])

    def test_dry_run_needs_no_credentials_or_network(self):
        result = subprocess.run([
            sys.executable, str(HERE / "apply-cache-rules.py"),
            "--zone-id", "0" * 32, "--dry-run",
        ], check=True, capture_output=True, text=True)
        self.assertEqual(json.loads(result.stdout), DESIRED)


if __name__ == "__main__":
    unittest.main()
