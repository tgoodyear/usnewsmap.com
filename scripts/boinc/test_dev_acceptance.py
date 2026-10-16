"""Offline review-input and dev-only DNS isolation checks."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("prepare_dev_acceptance", Path(__file__).with_name("prepare_dev_acceptance.py"))
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)


class DevAcceptance(unittest.TestCase):
    def settings(self):
        return PLAN.inputs("usnmboinccrdevabc123", "0b398b81-9e13-427f-9dc2-e38d890554bc", "Operator")

    def load(self, value, env="dev"):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "settings.json")
            path.write_text(json.dumps(value, sort_keys=True))
            command = ('die() { echo "$*" >&2; exit 1; }; ENV_NAME=prod; '
                       '. scripts/lib/boinc.sh; ENV_NAME="$TEST_ENV"; BOINC_SETTINGS="$SETTINGS"; boinc_load_settings; '
                       'printf "%s\\n" "$BOINC_DNS_ZONE" "$BOINC_DNS_RG" "$MASTER_HOST"')
            return subprocess.run(["bash", "-c", command], cwd=ROOT, capture_output=True, text=True,
                                  env={**os.environ, "TEST_ENV": env, "SETTINGS": str(path)})

    def test_exact_artifact_pair_and_no_production_registry(self):
        files = self.settings()
        runtime = files["runtime-settings.json"]
        self.assertEqual(files["names.json"]["rg"], "usnm-boinc-rg-dev")
        self.assertEqual(runtime["images"]["server"].split("@")[1], PLAN.SERVER)
        self.assertEqual(runtime["images"]["bridge"].split("@")[1], PLAN.BRIDGE)
        self.assertEqual(runtime["images"]["worker"], PLAN.WORKER)
        self.assertEqual(runtime["images"]["appver"], "")
        self.assertFalse(runtime["goldBundle"])
        self.assertTrue(all(not value for value in files["base-settings.json"]["images"].values()))
        with self.assertRaises(ValueError):
            PLAN.inputs("crusnmprodffapzy", runtime["operator"]["objectId"], "Operator")

    def test_dev_dns_authority_is_separate_from_registry(self):
        result = self.load(self.settings()["runtime-settings.json"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["usnewsmap.com", "rg-usnm-prod", "boinc-dev.usnewsmap.com"])

    def test_prod_override_is_refused(self):
        value = self.settings()["runtime-settings.json"]
        value["projectUrl"] = "https://boinc.usnewsmap.com/"
        result = self.load(value, env="prod")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dev only", result.stderr)

    def test_existing_prod_defaults_are_unchanged(self):
        value = json.loads(Path(ROOT, "contrib-compute/infra/boinc/env/prod.json").read_text())
        result = self.load(value, env="prod")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["", "", "boinc.usnewsmap.com"])

    def test_bad_dns_is_refused(self):
        for dns in (None, {}, {"zone": "usnewsmap.com", "resourceGroup": "rg-usnm-prod", "extra": True},
                    {"zone": 1, "resourceGroup": "rg-usnm-prod"},
                    {"zone": "usnewsmap.com", "resourceGroup": "../prod"}):
            with self.subTest(dns=dns):
                value = copy.deepcopy(self.settings()["runtime-settings.json"])
                value["dns"] = dns
                self.assertNotEqual(self.load(value).returncode, 0)

    def test_output_cannot_overwrite_settings_or_external_paths(self):
        script = Path(__file__).with_name("prepare_dev_acceptance.py")
        result = subprocess.run(["python3", str(script), "--registry-name", "usnmboinccrdevabc123",
                                 "--operator-id", "0b398b81-9e13-427f-9dc2-e38d890554bc",
                                 "--operator-login", "Operator", "--output-dir", "contrib-compute/infra/boinc/env"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("beneath checkout target/", result.stderr)

    def test_documented_teardown_stops_before_group_delete_on_stack_errors(self):
        doc = Path(ROOT, "contrib-compute/docs/dev-acceptance.md").read_text()
        teardown = doc.split("## Teardown is part of the approval", 1)[1]
        command = re.search(r"```sh\n(.*?)\n```", teardown, re.S).group(1)
        for phase in ("stack group show", "stack group delete"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                az = Path(directory, "az")
                az.write_text('#!/bin/bash\nprintf "%s\\n" "$*" >> "$AZ_LOG"\n'
                              'if [[ "$*" == "$FAIL_PHASE "* ]]; then exit 9; fi\n')
                az.chmod(0o755)
                log = Path(directory, "az.log")
                result = subprocess.run(["bash", "-c", command], capture_output=True, text=True,
                                        env={**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                                             "AZ_LOG": str(log), "FAIL_PHASE": phase, "SUB": "offline-test"})
                self.assertEqual(result.returncode, 9, result.stderr)
                self.assertFalse(any(line.startswith("group delete") for line in log.read_text().splitlines()))


if __name__ == "__main__":
    unittest.main()
