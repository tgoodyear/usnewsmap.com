"""Exercise deployment ordering and failure gates without Azure or project mutation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SUB = json.loads((ROOT / "contrib-compute/infra/boinc/env/prod.json").read_text())["subscriptionId"]

AZ = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$*" in
  "account show"*) echo "$STUB_SUB" ;;
  "group show"*) ;;
  "identity list"*) echo '[{"name":"usnm-boinc-id-prod","principalId":"11111111-1111-1111-1111-111111111111"},{"name":"usnm-boinc-secrets-id-prod","principalId":"22222222-2222-2222-2222-222222222222"}]' ;;
  "stack sub show"*) echo '{"OCR_EXCHANGE_ACCOUNT":{"value":"exchangeprod"}}' ;;
  "storage blob exists"*) echo true ;;
  "storage blob lease acquire"* | "storage blob lease release"*) ;;
  "stack group show"*) echo '{"appFqdn":{"value":"example.azurecontainerapps.io"},"secretsJobName":{"value":"secrets"},"initJobName":{"value":"init"},"bridgeJobName":{"value":"bridge"},"publishJobName":{"value":"publish"}}' ;;
  "containerapp env certificate list"*) echo Succeeded ;;
  "stack-whatif group create"*"--help"*) ;;
  "stack-whatif group create"*) exit "${STUB_PREVIEW_STATUS:-0}" ;;
  "stack group create"*) ;;
  "containerapp job execution list"*) ;;
  "containerapp job show"*) echo '{"containers":[{"name":"bridge","env":[{"name":"AZURE_CLIENT_ID","value":"client"},{"name":"KEY_VAULT_NAME","value":"vault"},{"name":"DB_HOST","value":"db.example.org"},{"name":"DB_NAME","value":"usnmocr"},{"name":"MASTER_HOST","value":"boinc.usnewsmap.com"},{"name":"PROJECT_DIR","value":"/project"},{"name":"HANDOFF_DIR","value":"/project/handoff"}]}]}' ;;
  "containerapp job update"*) ;;
  "containerapp job start"*)
    while [ $# -gt 0 ]; do
      if [ "$1" = --name ]; then echo "$2-execution"; break; fi
      shift
    done ;;
  "containerapp job execution show"*"--query properties.status"*)
    case "$*" in
      *"--name init "*) echo "${STUB_INIT_STATUS:-Succeeded}" ;;
      *"--name publish "*) echo "${STUB_PUBLISH_STATUS:-Succeeded}" ;;
      *) echo Succeeded ;;
    esac ;;
  "containerapp job execution show"*"properties.template"*)
    grep '^containerapp job update' "$STUB_LOG" | tail -n 1 |
      sed -e 's/.*--replace-env-vars //' -e 's/ -o none.*//' | tr ' ' '\n' |
      jq -R 'split("=") | {name: .[0], value: (.[1:] | join("="))}' | jq -s . ;;
  *) echo "unexpected az call: $*" >&2; exit 9 ;;
esac
"""


class Provision(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        directory = Path(self.tmp.name)
        self.log = directory / "az.log"
        for name, content in [
            ("az", AZ),
            ("curl", '#!/bin/sh\nprintf "%s\\n" "$STUB_HEALTH"\n'),
        ]:
            path = directory / name
            path.write_text(content)
            path.chmod(0o755)
        self.env = {
            **os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"],
            "STUB_LOG": str(self.log), "STUB_SUB": SUB,
            "USNM_BOINC_LOCK_DIR": str(directory / "provision.lock"),
            "STUB_HEALTH": '<project_config><master_url>https://boinc.usnewsmap.com/</master_url>'
                           '<web_stopped>0</web_stopped><sched_stopped>0</sched_stopped></project_config>',
        }

    def run_script(self, *args, **env):
        return subprocess.run(
            ["bash", "scripts/provision-boinc.sh", "--env", "prod", *args], cwd=ROOT,
            env={**self.env, **env}, capture_output=True, text=True,
        )

    def calls(self):
        return self.log.read_text().splitlines()

    def test_stack_preview_includes_same_destructive_and_deny_settings_without_deploying(self):
        result = self.run_script("--what-if")
        self.assertEqual(result.returncode, 0, result.stderr)
        [preview] = [call for call in self.calls() if call.startswith("stack-whatif") and "--help" not in call]
        self.assertIn("--action-on-unmanage deleteResources", preview)
        self.assertIn("--deny-settings-mode denyDelete", preview)
        self.assertIn("--deny-settings-excluded-actions", preview)
        self.assertIn("--retention-interval PT3H", preview)
        self.assertIn(f"--stack-id /subscriptions/{SUB}/resourceGroups/usnm-boinc-rg-prod/", preview)
        self.assertFalse(any(call.startswith(("stack group create", "containerapp job start", "deployment group what-if")) for call in self.calls()))

    def test_preview_failure_stops_before_deployment(self):
        result = self.run_script(STUB_PREVIEW_STATUS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call.startswith("stack group create") for call in self.calls()))

    def test_workbook_smoke_serializes_hosts_and_preserves_operator_override(self):
        script = (ROOT / "scripts/provision-boinc.sh").read_text()
        smoke = script[script.index("workspace=$(baz monitor log-analytics workspace list"):]
        setup = """
set -eu
BOINC_RG=fixture SUBSCRIPTION=fixture
baz() { echo fixture; }
boinc_workdir() { echo "$FIXTURE_DIR"; }
boinc_output() {
  if [ "$1" = logAnalyticsQueryHosts ]; then
    echo '["westus.api.loganalytics.io","eastus.api.loganalytics.io"]'
  else echo westus2.api.loganalytics.io; fi
}
python3() { printf '%s' "$LOG_ANALYTICS_QUERY_HOSTS" > "$FIXTURE_DIR/hosts"; echo '{}'; }
"""
        for override in (None, "eastus.api.loganalytics.io", ""):
            env = {**self.env, "FIXTURE_DIR": self.tmp.name}
            env.pop("LOG_ANALYTICS_QUERY_HOSTS", None)
            if override is not None:
                env["LOG_ANALYTICS_QUERY_HOSTS"] = override
            result = subprocess.run(["bash", "-c", setup + smoke], cwd=ROOT,
                                    env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((Path(self.tmp.name) / "hosts").read_text(),
                             override if override is not None else
                             "westus.api.loganalytics.io,eastus.api.loganalytics.io")
        self.assertIn("output LOG_ANALYTICS_QUERY_HOSTS string = join(",
                      (ROOT / "infra/main.bicep").read_text())

    def test_automates_lifecycle_in_order_and_checks_public_configuration(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        starts = [call for call in self.calls() if call.startswith("containerapp job start")]
        self.assertEqual([call.split("--name ")[1].split()[0] for call in starts],
                         ["secrets", "init", "bridge", "publish"])
        self.assertIn("deployment ready", result.stdout)
        self.assertNotIn("by hand", result.stdout)
        self.assertNotIn("Directory Readers", result.stdout)

    def test_failed_init_or_publish_never_reports_readiness(self):
        for variable in ["STUB_INIT_STATUS", "STUB_PUBLISH_STATUS"]:
            result = self.run_script(**{variable: "Failed"})
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("deployment ready", result.stdout)

    def test_rejects_placeholder_foreign_or_stopped_project(self):
        for health in [
            "not initialised",
            self.env["STUB_HEALTH"].replace("boinc.usnewsmap.com", "example.org"),
            self.env["STUB_HEALTH"].replace("<sched_stopped>0", "<sched_stopped>1"),
        ]:
            result = self.run_script(STUB_HEALTH=health)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("deployment ready", result.stdout)


class ProjectInitialization(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.project = self.root / "project/usnmocr"
        self.project.mkdir(parents=True)
        lib = self.root / "lib.sh"
        lib.write_text(
            f'PROJ="{self.project}"\nSHARE_DIR="{self.project.parent}"\n'
            f'SECRETS="{self.root}/secrets"\n'
            'PROJECT=usnmocr\n'
            'fail() { echo "$*" >&2; exit 1; }\nlog() { echo "$*"; }\n'
        )
        source = ROOT / "contrib-compute/images/server/entrypoint.sh"
        self.script = self.root / "entrypoint.sh"
        self.script.write_text(source.read_text().replace("/usr/local/lib/usnm-boinc/lib.sh", str(lib)))

    def run_script(self, mode):
        return subprocess.run(["bash", str(self.script), mode], capture_output=True, text=True)

    def test_completed_initialization_is_idempotent_and_explicit_init_still_refuses(self):
        (self.project / ".usnm-initialised").touch()
        (self.project / "html/inc").mkdir(parents=True)
        (self.project / "html/inc/boinc_db.inc").touch()
        (self.project / "bin").mkdir()
        xadd = self.project / "bin/xadd"
        xadd.write_text("#!/bin/sh\nexit 99\n")
        xadd.chmod(0o755)
        result = self.run_script("--ensure-project")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already initialised", result.stdout)
        self.assertNotEqual(self.run_script("--init-project").returncode, 0)

    def test_partial_project_or_broken_marker_never_triggers_destructive_recovery(self):
        (self.project / "existing-data").write_text("preserve")
        result = self.run_script("--ensure-project")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing destructive", result.stderr)
        self.assertEqual((self.project / "existing-data").read_text(), "preserve")
        (self.project / ".usnm-initialised").touch()
        result = self.run_script("--ensure-project")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tree is incomplete", result.stderr)
        source = (ROOT / "contrib-compute/images/server/entrypoint.sh").read_text()
        self.assertNotIn("--drop_db_first", source)
        self.assertNotIn("--delete_prev_inst", source)


if __name__ == "__main__":
    unittest.main()
