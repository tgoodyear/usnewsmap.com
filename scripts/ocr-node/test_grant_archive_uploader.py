"""Archive-only rollout checks against a stub Azure CLI; no live changes."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).parent
PRINCIPAL = "162eafc4-20f8-4044-a4f1-2fe97f409d47"


class GrantTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.log = self.root / "calls"
        stub = self.root / "az"
        stub.write_text("""#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ["CALLS"], "a") as f: f.write(json.dumps(a)+"\\n")
if a[:2] == ["resource","list"]:
 print(json.dumps([{"identity":{"principalId": os.environ["PRINCIPAL"]}}]))
elif a[:3] == ["role","definition","list"]:
 p={"actions":[],"dataActions":[
 "Microsoft.Storage/storageAccounts/blobServices/containers/blobs/read",
 "Microsoft.Storage/storageAccounts/blobServices/containers/blobs/write"]}
 if os.environ.get("BAD_ROLE"): p["dataActions"].append("*")
 if os.environ.get("EXCLUDE_WRITE"): p["notDataActions"]=[p["dataActions"][1]]
 print(json.dumps([{"roleName":"usnm-scans-writer-role-prod (scope)",
 "id":"/subscriptions/test/providers/Microsoft.Authorization/roleDefinitions/role",
 "permissions":[p]}]))
elif a[:2] == ["deployment","group"]:
 sys.exit(int(os.environ.get("DEPLOY_FAIL","0")))
else: sys.exit(99)
""")
        stub.chmod(0o755)
        script = (HERE / "grant-archive-uploader.sh").read_text()
        # Isolate private settings lookup; exercise the actual command/validation body.
        start = script.index('ROOT=$(cd ')
        end = script.index('SUBSCRIPTION=$(aget ')
        settings = """
aget() {
 case "$1" in
 AZURE_SUBSCRIPTION_ID) echo 25bf257c-c94e-4d61-bba3-edc635f46602 ;;
 AZURE_RESOURCE_GROUP) echo rg-usnm-prod ;;
 SCAN_ARCHIVE_ACCOUNT) echo usnmscansstprodffapzy ;;
 USNM_SCAN_ARCHIVE_UPLOADER_ID) echo "$PRINCIPAL" ;;
 esac
}
"""
        self.script = self.root / "grant.sh"
        self.script.write_text(script[:start] + settings + script[end:])
        self.env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ["PATH"],
                        CALLS=str(self.log), PRINCIPAL=PRINCIPAL)

    def run_it(self, *args, **extra):
        return subprocess.run(["bash", str(self.script), "--env", "prod", *args],
                              env=dict(self.env, **extra), capture_output=True,
                              text=True, check=False)

    def test_preview_only_deploys_one_narrow_template(self):
        result = self.run_it("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls[-1][:3], ["deployment", "group", "what-if"])
        self.assertIn("scripts/ocr-node/scan-uploader-grant.bicep", calls[-1])
        self.assertNotIn("main.bicep", " ".join(calls[-1]))

    def test_create_uses_same_template_and_principal(self):
        result = self.run_it()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls[-1][:3], ["deployment", "group", "create"])
        params = json.loads(calls[-1][-1])
        self.assertEqual(params["principal"]["value"], PRINCIPAL)
        self.assertTrue(params["scope"]["value"].endswith("/containers/scans"))

    def test_excess_role_permissions_never_deploy(self):
        result = self.run_it(BAD_ROLE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('"deployment"', self.log.read_text())

    def test_deployment_error_is_not_hidden(self):
        self.assertNotEqual(self.run_it(DEPLOY_FAIL="7").returncode, 0)

    def test_excluded_write_never_deploys(self):
        self.assertNotEqual(self.run_it(EXCLUDE_WRITE="1").returncode, 0)
        self.assertNotIn('"deployment"', self.log.read_text())

    def test_invalid_identity_never_calls_azure(self):
        self.assertNotEqual(self.run_it(PRINCIPAL="bad").returncode, 0)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
