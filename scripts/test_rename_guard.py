"""Offline tests for ADR-0017's protected-resource removal gate."""

import json
import subprocess
import unittest
from pathlib import Path

from scripts.lib.rename_guard import PROTECTED_TYPES, check

ROOT = Path(__file__).resolve().parents[1]
BASE = "/subscriptions/sub-1/resourceGroups/rg-usnm-prod/providers/"


class RenameGuard(unittest.TestCase):
    def plan(self, kind="Ignore", resource_type="Microsoft.Storage/storageAccounts", name="old"):
        rid = BASE + resource_type + "/" + name
        return [{"id": rid}], {"status": "Succeeded", "changes": [{"resourceId": rid, "changeType": kind}]}

    def test_every_protected_type(self):
        self.assertEqual(len(PROTECTED_TYPES), 9)
        for resource_type in PROTECTED_TYPES:
            with self.subTest(resource_type=resource_type):
                resources, plan = self.plan(resource_type=resource_type)
                self.assertEqual(len(check(resources, plan)), 1)
                self.assertEqual(check(resources, plan, ["old"]), [])

    def test_ignore_delete_and_removal_without_replacement(self):
        for kind in ("Ignore", "Delete"):
            resources, plan = self.plan(kind)
            self.assertEqual(len(check(resources, plan)), 1)
            self.assertEqual(len(check(resources, plan, ["ol", "old*"])), 1)
            self.assertEqual(check(resources, plan, ["OLD"]), [])

    def test_retained_resources_and_unmanaged_resources(self):
        for kind in ("Create", "Modify", "NoChange", "Deploy"):
            self.assertEqual(check(*self.plan(kind)), [])
        resources, plan = self.plan()
        self.assertEqual(check([], plan), [])
        for resource_type in ("Microsoft.App/jobs", "Microsoft.Resources/deployments",
                              "Microsoft.Storage/storageAccounts/parent/blobServices",
                              "Microsoft.Authorization/policyAssignments"):
            self.assertEqual(check(*self.plan(resource_type=resource_type)), [])
        self.assertEqual(check([{"id": "/subscriptions/sub-1/resourceGroups/rg-usnm-prod"}],
                               {"changes": []}), [])

    def test_missing_and_unsupported_fail_closed_even_with_override(self):
        resources, plan = self.plan("Unsupported")
        self.assertEqual(len(check(resources, plan, ["old"])), 1)
        plan["changes"] = []
        self.assertIn("missing", check(resources, plan, ["old"])[0])

    def test_case_insensitive_id_and_exact_name(self):
        resources, plan = self.plan()
        plan["changes"][0]["resourceId"] = resources[0]["id"].upper() + "/"
        self.assertEqual(check(resources, plan, ["old"]), [])

    def test_all_deletions_need_individual_permission(self):
        r1, p1 = self.plan(name="one")
        r2, p2 = self.plan(name="two")
        plan = {"changes": p1["changes"] + p2["changes"]}
        self.assertEqual(len(check(r1 + r2, plan, ["one"])), 1)
        self.assertEqual(check(r1 + r2, plan, ["one", "two"]), [])

    def test_malformed_or_failed_inputs(self):
        resources, plan = self.plan()
        for inventory, response in ((None, plan), ({}, plan), ([{}], plan),
                                    (resources, {}), (resources, {"changes": None}),
                                    (resources, {"changes": [], "status": "Failed"}),
                                    (resources, {"changes": [{}]}),
                                    (resources, {"changes": plan["changes"] * 2})):
            with self.subTest(inventory=inventory, response=response), self.assertRaises(ValueError):
                check(inventory, response, ["old"])

    def test_cli_stream_and_repeated_overrides(self):
        resources, plan = self.plan()
        script = ROOT / "scripts/lib/rename_guard.py"
        for args, status in (([], 1), (["--allow-rename=old", "--allow-rename=another"], 0),
                             (["--allow-rename=*"], 2), (["--allow-rename="], 2)):
            run = subprocess.run(["python3", str(script), *args], input=json.dumps(resources) + "\n" +
                                 json.dumps(plan, indent=2), capture_output=True, text=True)
            self.assertEqual(run.returncode, status, run.stderr)
        run = subprocess.run(["python3", str(script)], input="not json", capture_output=True, text=True)
        self.assertEqual(run.returncode, 1)

    def test_mixed_stack_inventory_ignores_groups_and_children_only(self):
        resources, plan = self.plan("NoChange")
        resources += [{"id": "/subscriptions/sub-1/resourceGroups/rg-usnm-prod"},
                      {"id": self.plan()[0][0]["id"] + "/blobServices/default"}]
        self.assertEqual(check(resources, plan), [])


if __name__ == "__main__":
    unittest.main()
