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
  "stack group show"*)
    if [ "${STUB_STACK_SHOW_FAIL_AFTER:-0}" -gt 0 ]; then
      count=$(cat "$STUB_LOG.stack_show_count" 2>/dev/null || echo 0)
      count=$((count + 1))
      echo "$count" > "$STUB_LOG.stack_show_count"
      [ "$count" -lt "$STUB_STACK_SHOW_FAIL_AFTER" ] || { echo "transient stack show error" >&2; exit 1; }
    fi
    echo '{"appFqdn":{"value":"example.azurecontainerapps.io"},"secretsJobName":{"value":"secrets"},"initJobName":{"value":"init"},"bridgeJobName":{"value":"bridge"},"publishJobName":{"value":"publish"},"goldBundleJobName":{"value":"gold"},"keyVaultName":{"value":"vault"},"mysqlHost":{"value":"db.example.org"}}' ;;
  "containerapp env certificate list"*) echo Succeeded ;;
  "stack-whatif group create"*"--help"*) ;;
  "stack-whatif group create"*) exit "${STUB_PREVIEW_STATUS:-0}" ;;
  "stack group create"*) ;;
  "containerapp job execution list"*)
    if [[ "$*" == *"--name quality "* ]] || [[ "$*" == *"--name usnm-boinc-quality-caj-prod "* ]]; then
      [ "${STUB_QUALITY_ACTIVE:-0}" != 1 ] || echo quality-existing-execution
    elif [[ "$*" == *"--name publish "* ]]; then
      [ "${STUB_PUBLISH_ACTIVE:-0}" != 1 ] || echo publish-existing-execution
    elif [[ "$*" == *"--name gold "* ]]; then
      [ "${STUB_GOLD_ACTIVE:-0}" != 1 ] || echo gold-existing-execution
    elif [[ "$*" == *"--name bridge "* ]]; then
      [ "${STUB_REGISTRATION_LIST_FAIL:-0}" != 1 ] || exit 1
      if [ "${STUB_REGISTRATION_ACTIVE:-0}" = 1 ] || [ "${STUB_BRIDGE_ACTIVE:-0}" = 1 ]; then
        echo bridge-existing-execution
      fi
    fi ;;
  "containerapp job show"*)
    extra_secret='{"name":"PRIVATE_SETTING","secretRef":"runtime-secret"},'
    [ "${STUB_BRIDGE_CLEAN_ENV:-0}" != 1 ] || extra_secret=""
    storage_name=${STUB_DEPLOYED_STORAGE_NAME:-project}
    mount_options=${STUB_DEPLOYED_MOUNT_OPTIONS:-mfsymlinks,uid=1000,gid=1000,dir_mode=0775,file_mode=0775,cache=strict}
    if [[ "$*" == *"--name gold "* ]]; then
      echo "{\"volumes\":[{\"name\":\"project\",\"storageType\":\"AzureFile\",\"storageName\":\"$storage_name\",\"mountOptions\":\"$mount_options\"}],\"containers\":[{\"name\":\"bridge\",\"image\":\"${STUB_DEPLOYED_GOLD_IMAGE:-fixture@sha256:unchanged}\",\"resources\":{\"cpu\":1,\"memory\":\"2Gi\"},\"volumeMounts\":[{\"volumeName\":\"project\",\"mountPath\":\"/project\"}],\"env\":[{\"name\":\"BRIDGE_STEP\",\"value\":\"${STUB_DEPLOYED_GOLD_STEP:-gold-bundle}\"},{\"name\":\"GOLD_CAMPAIGN\",\"value\":\"v1-tess550\"},${extra_secret}{\"name\":\"AZURE_CLIENT_ID\",\"value\":\"client\"},{\"name\":\"KEY_VAULT_NAME\",\"value\":\"vault\"},{\"name\":\"DB_HOST\",\"value\":\"db.example.org\"},{\"name\":\"DB_NAME\",\"value\":\"usnmocr\"},{\"name\":\"MASTER_HOST\",\"value\":\"boinc.usnewsmap.com\"},{\"name\":\"PROJECT_DIR\",\"value\":\"/project\"},{\"name\":\"HANDOFF_DIR\",\"value\":\"/project/handoff\"}]}]}"
    else
      echo "{\"volumes\":[{\"name\":\"project\",\"storageType\":\"AzureFile\",\"storageName\":\"$storage_name\",\"mountOptions\":\"$mount_options\"}],\"containers\":[{\"name\":\"bridge\",\"image\":\"${STUB_DEPLOYED_BRIDGE_IMAGE:-fixture@sha256:unchanged}\",\"resources\":{\"cpu\":0.5,\"memory\":\"1Gi\"},\"volumeMounts\":[{\"volumeName\":\"project\",\"mountPath\":\"/project\"}],\"env\":[{\"name\":\"BRIDGE_STEP\",\"value\":\"${STUB_DEPLOYED_BRIDGE_STEP:-iris-bundle-collect}\"},{\"name\":\"IRIS_CAMPAIGN\",\"value\":\"overproof-100-v12\"},${extra_secret}{\"name\":\"AZURE_CLIENT_ID\",\"value\":\"client\"},{\"name\":\"KEY_VAULT_NAME\",\"value\":\"vault\"},{\"name\":\"DB_HOST\",\"value\":\"db.example.org\"},{\"name\":\"DB_NAME\",\"value\":\"usnmocr\"},{\"name\":\"MASTER_HOST\",\"value\":\"boinc.usnewsmap.com\"},{\"name\":\"PROJECT_DIR\",\"value\":\"/project\"},{\"name\":\"HANDOFF_DIR\",\"value\":\"/project/handoff\"}]}]}"
    fi ;;
  "containerapp job update"*) ;;
  "containerapp job start"*)
    if [[ "$*" == *"--yaml"* ]] && [ "${STUB_REGISTRATION_START_FAIL:-0}" = 1 ]; then exit 1; fi
    if [[ "$*" == *"--yaml"* ]]; then
      for ((i=1; i<=$#; i++)); do
        if [ "${!i}" = --yaml ]; then
          j=$((i+1)); cp "${!j}" "$STUB_LOG.registration.json"; break
        fi
      done
    fi
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


    def test_registration_uses_complete_execution_override_without_replacing_iris_defaults(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call.startswith("containerapp job update") for call in self.calls()))
        template = json.loads(Path(str(self.log) + ".registration.json").read_text())
        [container] = template["containers"]
        env = {v["name"]: v.get("value") for v in container["env"]}
        self.assertEqual(env["BRIDGE_STEP"], "register-apps")
        self.assertEqual(env["IRIS_CAMPAIGN"], "overproof-100-v12")
        self.assertIn({"name": "PRIVATE_SETTING", "secretRef": "runtime-secret"}, container["env"])
        self.assertEqual(container["image"], "fixture@sha256:unchanged")
        self.assertEqual(container["resources"], {"cpu": 0.5, "memory": "1Gi"})
        self.assertEqual(container["volumeMounts"], [{"volumeName": "project", "mountPath": "/project"}])
        self.assertEqual(template["volumes"][0]["name"], "project")
        acquires = [i for i, c in enumerate(self.calls()) if c.startswith("storage blob lease acquire")]
        starts = [i for i, c in enumerate(self.calls()) if c.startswith("containerapp job start") and "--yaml" in c]
        releases = [i for i, c in enumerate(self.calls()) if c.startswith("storage blob lease release")]
        self.assertEqual(len(acquires), 1)
        self.assertEqual(len(releases), 1)
        self.assertLess(acquires[0], starts[0])
        self.assertLess(starts[0], releases[0])

    def test_registration_preflight_failures_release_lease_without_starting(self):
        for variable in ("STUB_REGISTRATION_LIST_FAIL", "STUB_REGISTRATION_ACTIVE"):
            with self.subTest(variable=variable):
                self.log.write_text("")
                result = self.run_script(**{variable: "1"})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(call.startswith("containerapp job start") and "--name bridge" in call
                                     for call in self.calls()))
                self.assertEqual(sum(call.startswith("storage blob lease acquire") for call in self.calls()), 1)
                self.assertEqual(sum(call.startswith("storage blob lease release") for call in self.calls()), 1)
                self.assertNotIn("lease retained", result.stderr)

    def test_uncertain_registration_start_retains_lease_for_reconciliation(self):
        result = self.run_script(STUB_REGISTRATION_START_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue(any(call.startswith("containerapp job start") and "--yaml" in call
                            for call in self.calls()))
        self.assertFalse(any(call.startswith("storage blob lease release") for call in self.calls()))

    def test_failed_init_or_publish_never_reports_readiness(self):
        for variable in ["STUB_INIT_STATUS", "STUB_PUBLISH_STATUS"]:
            result = self.run_script(**{variable: "Failed"})
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("deployment ready", result.stdout)

    def test_running_quality_job_does_not_block_appver_publish_or_server_rollout(self):
        appver = "crusnmprodffapzy.azurecr.io/usnm-boinc-appver@sha256:" + "a" * 64
        for args in (
            ("--publish-image", appver),
            (),
        ):
            with self.subTest(args=args):
                self.log.write_text("")
                result = self.run_script(*args, STUB_QUALITY_ACTIVE="1")
                self.assertEqual(result.returncode, 0, result.stderr)
                starts = [call.split("--name ")[1].split()[0]
                          for call in self.calls() if call.startswith("containerapp job start")]
                self.assertEqual(starts, ["secrets", "init", "bridge", "publish"])
                self.assertFalse(any("quality" in call for call in self.calls()))

    def test_running_bridge_job_blocks_only_bridge_template_update(self):
        committed_bridge = json.loads((ROOT / "contrib-compute/infra/boinc/env/prod.json").read_text())["images"]["bridge"]
        appver = "crusnmprodffapzy.azurecr.io/usnm-boinc-appver@sha256:" + "a" * 64
        for args in (
            ("--publish-image", appver),
            (),
        ):
            with self.subTest(non_conflicting=args):
                self.log.write_text("")
                result = self.run_script(*args, STUB_BRIDGE_ACTIVE="1",
                                         STUB_BRIDGE_CLEAN_ENV="1",
                                         STUB_DEPLOYED_BRIDGE_IMAGE=committed_bridge)
                self.assertEqual(result.returncode, 0, result.stderr)
                starts = [call.split("--name ")[1].split()[0]
                          for call in self.calls() if call.startswith("containerapp job start")]
                self.assertEqual(starts, ["secrets", "init", "publish"])
                self.assertEqual(sum(call.startswith("storage blob lease acquire") for call in self.calls()), 1)
                self.assertEqual(sum(call.startswith("storage blob lease release") for call in self.calls()), 1)
                self.assertIn("deferring bridge registration while bridge-existing-execution is running",
                              result.stdout)

        for env in (
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_BRIDGE_IMAGE": "fixture@sha256:stale"},
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_BRIDGE_IMAGE": committed_bridge,
             "STUB_DEPLOYED_BRIDGE_STEP": "submit"},
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_BRIDGE_IMAGE": committed_bridge,
             "STUB_DEPLOYED_STORAGE_NAME": "other-project"},
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_BRIDGE_IMAGE": committed_bridge,
             "STUB_DEPLOYED_MOUNT_OPTIONS": "mfsymlinks,uid=1000,gid=1000"},
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_BRIDGE_IMAGE": committed_bridge,
             "STUB_STACK_SHOW_FAIL_AFTER": "7"},
            {"STUB_DEPLOYED_BRIDGE_IMAGE": committed_bridge},
        ):
            with self.subTest(conflicting_env=env):
                self.log.write_text("")
                Path(str(self.log) + ".stack_show_count").unlink(missing_ok=True)
                result = self.run_script(STUB_BRIDGE_ACTIVE="1", **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("bridge already has an active execution", result.stderr)
                self.assertEqual(sum(call.startswith("storage blob lease acquire") for call in self.calls()), 1)
                self.assertEqual(sum(call.startswith("storage blob lease release") for call in self.calls()), 1)
                self.assertFalse(any(call.startswith(("stack group create", "containerapp job start"))
                                     for call in self.calls()))

        self.log.write_text("")
        result_publish = self.run_script("--publish-image", "", STUB_PUBLISH_ACTIVE="1")
        self.assertNotEqual(result_publish.returncode, 0)
        self.assertIn("publish already has an active execution", result_publish.stderr)
        self.assertFalse(any(call.startswith(("stack group create", "containerapp job start"))
                             for call in self.calls()))

        self.log.write_text("")
        result_gold_ok = self.run_script("--publish-image", appver, STUB_GOLD_ACTIVE="1",
                                         STUB_BRIDGE_CLEAN_ENV="1",
                                         STUB_DEPLOYED_GOLD_IMAGE=committed_bridge)
        self.assertEqual(result_gold_ok.returncode, 0, result_gold_ok.stderr)
        starts = [call.split("--name ")[1].split()[0]
                  for call in self.calls() if call.startswith("containerapp job start")]
        self.assertEqual(starts, ["secrets", "init", "bridge", "publish"])

        for gold_env in (
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_GOLD_IMAGE": "fixture@sha256:stale"},
            {"STUB_BRIDGE_CLEAN_ENV": "1", "STUB_DEPLOYED_GOLD_IMAGE": committed_bridge,
             "STUB_DEPLOYED_GOLD_STEP": "submit"},
            {"STUB_DEPLOYED_GOLD_IMAGE": committed_bridge},
        ):
            with self.subTest(conflicting_gold_env=gold_env):
                self.log.write_text("")
                result_gold_conflict = self.run_script("--publish-image", appver, STUB_GOLD_ACTIVE="1", **gold_env)
                self.assertNotEqual(result_gold_conflict.returncode, 0)
                self.assertIn("gold already has an active execution", result_gold_conflict.stderr)
                self.assertFalse(any(call.startswith(("stack group create", "containerapp job start"))
                                     for call in self.calls()))

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


class TlsSettings(unittest.TestCase):
    def test_committed_boolean_and_legacy_default_are_preserved(self):
        settings = json.loads((ROOT / "contrib-compute/infra/boinc/env/prod.json").read_text())
        self.assertIs(settings["requireSecureTransport"], False)
        for value in (None, False, True):
            with self.subTest(value=value), tempfile.TemporaryDirectory(dir=ROOT) as directory:
                candidate = dict(settings)
                if value is None:
                    candidate.pop("requireSecureTransport")
                else:
                    candidate["requireSecureTransport"] = value
                path = Path(directory) / "settings.json"
                path.write_text(json.dumps(candidate, sort_keys=True))
                result = self.load(path)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), "true" if value is True else "false")

    def test_explicit_null_strings_numbers_and_objects_are_rejected(self):
        settings = json.loads((ROOT / "contrib-compute/infra/boinc/env/prod.json").read_text())
        for value in (None, "true", "false", 0, 1, {}, []):
            with self.subTest(value=value), tempfile.TemporaryDirectory(dir=ROOT) as directory:
                path = Path(directory) / "settings.json"
                path.write_text(json.dumps({**settings, "requireSecureTransport": value}, sort_keys=True))
                result = self.load(path)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("requireSecureTransport must be true or false", result.stderr)

    def load(self, path):
        return subprocess.run(
            ["bash", "-c", 'set -eu; ENV_NAME=prod; die() { echo "$*" >&2; exit 1; }; '
             '. scripts/lib/boinc.sh; BOINC_SETTINGS="$1"; boinc_load_settings; '
             'printf "%s\\n" "$REQUIRE_SECURE_TRANSPORT"', "fixture", str(path)],
            cwd=ROOT, capture_output=True, text=True,
        )

    def test_parameter_writer_forwards_boolean_not_string(self):
        source = (ROOT / "scripts/provision-boinc.sh").read_text()
        writer = source[source.index("write_params() {"):source.index("STAGE=$(domain_stage)")]
        for value in ("true", "false"):
            with self.subTest(value=value), tempfile.TemporaryDirectory(dir=ROOT) as directory:
                setup = (
                    'set -eu; runtime_principal=x; secrets_principal=y; ASSESSMENT=false; '
                    'GOLD_BUNDLE=false; DNS_ZONE=example.org; REGISTRY_RG=rg; REGISTRY_NAME=registry; '
                    'ENV_NAME=prod; LOCATION=westus2; SERVER_IMAGE=; BRIDGE_IMAGE=; PUBLISH_IMAGE=; '
                    'MASTER_HOST=boinc.usnewsmap.com; OPERATOR_ID=x; WEB_IMAGE=immutable-web; '
                    'BOINC_SETTINGS=contrib-compute/infra/boinc/env/prod.json; '
                    'work="$1"; REQUIRE_SECURE_TRANSPORT="$2"; '
                )
                result = subprocess.run(["bash", "-c", setup + writer + '\nwrite_params 2',
                                         "fixture", directory, value], cwd=ROOT,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                parameters = json.loads((Path(directory) / "params.json").read_text())["parameters"]
                self.assertIs(parameters["requireSecureTransport"]["value"], value == "true")
                self.assertEqual(parameters["webImage"]["value"], "immutable-web")


class ProductionReconciliation(unittest.TestCase):
    def test_optional_web_image_is_validated_and_loaded_without_changing_lifecycle_images(self):
        settings = json.loads((ROOT / "contrib-compute/infra/boinc/env/prod.json").read_text())
        web = settings["images"]["web"]
        for value in ("absent", "", web, None, False, 42, "mutable:latest"):
            with self.subTest(value=value), tempfile.TemporaryDirectory(dir=ROOT) as directory:
                candidate = json.loads(json.dumps(settings))
                if value == "absent":
                    candidate["images"].pop("web")
                else:
                    candidate["images"]["web"] = value
                path = Path(directory) / "settings.json"
                path.write_text(json.dumps(candidate, sort_keys=True))
                result = subprocess.run([
                    "bash", "-c", 'ENV_NAME=prod; die() { echo "$*" >&2; exit 1; }; '
                    '. scripts/lib/boinc.sh; BOINC_SETTINGS="$1"; boinc_load_settings; '
                    'printf "%s\\n%s\\n%s" "$WEB_IMAGE" "$SERVER_IMAGE" "$BRIDGE_IMAGE"',
                    "fixture", str(path)], cwd=ROOT, text=True, capture_output=True)
                if value in ("absent", "", web):
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.splitlines(), [
                        "" if value == "absent" else value,
                        settings["images"]["server"], settings["images"]["bridge"]])
                else:
                    self.assertNotEqual(result.returncode, 0)

    def test_build_replaces_additive_web_pin_with_rebuilt_bridge_containing_endpoint(self):
        source = (ROOT / "scripts/build-boinc-images.sh").read_text()
        expression = source.split('--arg b "$bridge_image" ')[1].split("'", 2)[1]
        for images in ({"server": "old", "bridge": "old"},
                       {"server": "old", "bridge": "old", "web": "additive"}):
            result = subprocess.run(["jq", "--arg", "s", "new-server", "--arg", "b", "new-bridge",
                                     expression], input=json.dumps({"images": images}),
                                    text=True, capture_output=True, check=True)
            actual = json.loads(result.stdout)["images"]
            self.assertEqual(actual["server"], "new-server")
            self.assertEqual(actual["bridge"], "new-bridge")
            self.assertEqual(actual.get("web"), "new-bridge" if "web" in images else None)

    def test_live_iris_settings_quota_and_additive_endpoint_are_preserved(self):
        source = (ROOT / "contrib-compute/infra/boinc/main.bicep").read_text()
        bridge = source.split("module bridgeJob ")[1].split("module initJob ")[0]
        self.assertIn("value: 'iris-bundle-collect'", bridge)
        self.assertIn("value: 'overproof-100-v12'", bridge)
        self.assertIn("shareQuotaGb: 200", source)
        self.assertIn("!empty(webImage) ? webImage", source)
        self.assertIn("command: assessment || !empty(webImage)", source)
        self.assertTrue((ROOT / "contrib-compute/boinc/html/user/work_summary.php").is_file())
        # The separately operated quality runner is not adopted, restarted or rewritten by this stack.
        self.assertNotIn("usnm-boinc-quality", source)


if __name__ == "__main__":
    unittest.main()
