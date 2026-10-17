"""Tests for scripts/start-boinc-bridge.sh against a stub az.

The stub answers the calls the script makes and logs each one to STUB_LOG. STUB_TEMPLATE is
the job's template (JSON), STUB_EXEC_STATUS an execution's status, and STUB_NO_JOB=1 gives a
stack with no bridge job. The settings are the committed contrib-compute/infra/boinc/env/prod.json,
read only."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SCRIPT = os.path.join(HERE, "start-boinc-bridge.sh")
SUB = json.load(open(os.path.join(ROOT, "contrib-compute/infra/boinc/env/prod.json")))["subscriptionId"]

STUB_AZ = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$*" in
  "account show"*) echo "$STUB_SUB" ;;
  "group show"*) ;;
  "stack group show"*)
    if [ -n "${STUB_NO_JOB:-}" ]; then echo '{}'
    else echo '{"bridgeJobName": {"type": "String", "value": "usnm-boinc-bridge-caj-prod"}}'; fi ;;
  "containerapp job show"*) printf '%s\n' "$STUB_TEMPLATE" ;;
  "containerapp job update"*) ;;
  "containerapp job start"*) echo usnm-boinc-bridge-caj-prod-abc12 ;;
  "containerapp job execution list"*) printf '%s' "${STUB_RUNNING:-}" ;;
  "containerapp job execution show"*"--query properties.status"*) echo "${STUB_EXEC_STATUS:-Succeeded}" ;;
  "containerapp job execution show"*"properties.template"*)
    if [ -n "${STUB_EXEC_ENV:-}" ]; then printf '%s\n' "$STUB_EXEC_ENV"
    else grep '^containerapp job update' "$STUB_LOG" | tail -n 1 |
      sed -e 's/.*--replace-env-vars //' -e 's/ -o none.*//' | tr ' ' '\n' |
      jq -R 'split("=") | {name: .[0], value: (.[1:] | join("="))}' | jq -s .; fi ;;
  *) echo "stub az: unexpected call: $*" >&2; exit 9 ;;
esac
"""

BASE = {
    "AZURE_CLIENT_ID": "client-1",
    "KEY_VAULT_NAME": "usnmboinckvprod",
    "DB_HOST": "db.example.org",
    "DB_NAME": "usnmocr",
    "MASTER_HOST": "boinc.usnewsmap.com",
    "PROJECT_DIR": "/project",
    "HANDOFF_DIR": "/project/handoff",
}


def template(extra=None, drop=None):
    env = [{"name": k, "value": v} for k, v in BASE.items() if k != drop]
    env += [{"name": k, "value": v} for k, v in (extra or {}).items()]
    return json.dumps({"containers": [{"name": "bridge", "image": "x.azurecr.io/b@sha256:" + "0" * 64,
                                       "env": env, "volumeMounts": [{"volumeName": "project"}]}],
                       "volumes": [{"name": "project"}]})


class OperatorWorkspace(unittest.TestCase):
    def test_workspaces_are_unique_private_and_beneath_checkout(self):
        command = 'ENV_NAME=prod; die() { exit 1; }; . scripts/lib/boinc.sh; boinc_workdir; boinc_workdir'
        output = subprocess.check_output(["bash", "-c", command], cwd=ROOT, text=True)
        paths = [Path(line) for line in output.splitlines()]
        try:
            self.assertEqual(len(set(paths)), 2)
            for path in paths:
                self.assertEqual(path.parent, Path(ROOT))
                self.assertEqual(path.stat().st_mode & 0o777, 0o700)
        finally:
            for path in paths:
                shutil.rmtree(path)

    def test_failed_execution_log_is_printed_and_never_fails_the_caller(self):
        stub = r"""
az() {
  case "$*" in
    *"workspace list"*) echo ws-id ;;
    *"log-analytics query"*) [[ "$*" == *"'bad-exec-'"* ]] && echo '[{"TimeGenerated":"t1","Log_s":"Error: boom"}]' || echo '[]' ;;
  esac
}
"""
        command = (stub + "set -euo pipefail; ENV_NAME=prod; SUBSCRIPTION=s; die() { exit 1; }; "
                   ". scripts/lib/boinc.sh; boinc_execution_log bad-exec 0; boinc_execution_log quiet-exec 0; "
                   "boinc_execution_log 'x; rm' 0; echo done")
        result = subprocess.run(["bash", "-c", command], cwd=ROOT, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "done")
        self.assertIn("t1  Error: boom", result.stderr)
        self.assertIn("no log lines for quiet-exec yet", result.stderr)
        self.assertNotIn("x; rm", result.stderr)

    def test_release_staging_paths_match_image_build_paths(self):
        release = Path(ROOT, "scripts/boinc-app-release.sh").read_text()
        container = Path(ROOT, "contrib-compute/images/appver/Containerfile").read_text()
        self.assertIn('appver_binary="/opt/usnm-appver-build/wrapper-candidate/$binary_name"', release)
        self.assertIn("COPY wrapper-candidate /opt/usnm-appver-build/wrapper-candidate", container)
        for name in ("boinc-app-release.sh", "provision-boinc.sh", "build-boinc-images.sh"):
            text = Path(ROOT, "scripts", name).read_text()
            self.assertNotIn("mktemp", text)
            self.assertNotIn("/var/tmp", text)


class StartBridge(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        with open(os.path.join(bin_dir, "az"), "w") as f:
            f.write(STUB_AZ)
        os.chmod(os.path.join(bin_dir, "az"), 0o755)
        self.log = os.path.join(self.tmp, "az.log")
        self.lock_dir = os.path.join(self.tmp, "boinc.lock")
        self.env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "HOME": self.tmp,
                    "STUB_LOG": self.log, "STUB_SUB": SUB, "STUB_TEMPLATE": template(),
                    "USNM_BOINC_LOCK_DIR": self.lock_dir}

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_script(self, *args, env_name="prod", **extra):
        return subprocess.run([SCRIPT, "--env", env_name, *args], cwd=ROOT, env={**self.env, **extra},
                              capture_output=True, text=True)

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return f.read().splitlines()

    def update_vars(self):
        [call] = [c for c in self.calls() if c.startswith("containerapp job update")]
        tail = call.split("--replace-env-vars ", 1)[1].split(" -o none", 1)[0]
        return tail.split(" ")

    def test_sets_base_settings_and_step_then_starts_without_override(self):
        r = self.run_script("--step", "gold-prepare", "--var", "GOLD_CAMPAIGN=v1-tess550")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "usnm-boinc-bridge-caj-prod-abc12\n")
        self.assertEqual(self.update_vars(), [f"{k}={v}" for k, v in BASE.items()]
                         + ["BRIDGE_STEP=gold-prepare", "GOLD_CAMPAIGN=v1-tess550"])
        update = [c for c in self.calls() if c.startswith("containerapp job update")][0]
        self.assertIn("--container-name bridge", update)
        self.assertIn(f"--subscription {SUB}", update)
        [start] = [c for c in self.calls() if c.startswith("containerapp job start")]
        for flag in ("--env-vars", "--image", "--args", "--command", "--container-name"):
            self.assertNotIn(flag, start)
        order = [c.split(" ")[2] for c in self.calls() if c.startswith("containerapp job ")]
        self.assertEqual(order, ["show", "execution", "update", "start", "execution"])

    def test_drops_a_previous_runs_settings(self):
        r = self.run_script("--step", "register-apps",
                            STUB_TEMPLATE=template({"BRIDGE_STEP": "submit", "JOB_ID": "old"}))
        self.assertEqual(r.returncode, 0, r.stderr)
        vars_ = self.update_vars()
        self.assertIn("BRIDGE_STEP=register-apps", vars_)
        self.assertFalse([v for v in vars_ if v.startswith("JOB_ID=") or v == "BRIDGE_STEP=submit"])

    def test_refuses_to_override_the_jobs_own_settings(self):
        for bad in ("PROJECT_DIR=/tmp", "BRIDGE_STEP=submit", "lower=1", "NOEQUALS"):
            r = self.run_script("--step", "submit", "--var", bad)
            self.assertEqual(r.returncode, 1, bad)
        self.assertEqual(self.calls(), [])

    def test_refuses_a_repeated_var(self):
        r = self.run_script("--step", "submit", "--var", "JOB_ID=a", "--var", "JOB_ID=b")
        self.assertEqual(r.returncode, 1)
        self.assertIn("given twice", r.stderr)

    def test_stops_when_a_base_setting_is_missing(self):
        r = self.run_script("--step", "submit", STUB_TEMPLATE=template(drop="DB_HOST"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("no DB_HOST", r.stderr)
        self.assertFalse([c for c in self.calls() if "job update" in c or "job start" in c])

    def test_no_bridge_job(self):
        r = self.run_script("--step", "submit", STUB_NO_JOB="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no bridge job", r.stderr)

    def test_refuses_while_an_execution_runs(self):
        r = self.run_script("--step", "register-apps", STUB_RUNNING="usnm-boinc-bridge-caj-prod-old11\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("still running usnm-boinc-bridge-caj-prod-old11", r.stderr)
        self.assertFalse([c for c in self.calls() if "job update" in c or "job start" in c])

    def test_fails_when_the_execution_has_another_runs_settings(self):
        r = self.run_script("--step", "register-apps",
                            STUB_EXEC_ENV='[{"name": "BRIDGE_STEP", "value": "submit"}]')
        self.assertEqual(r.returncode, 1)
        self.assertIn("did not start with this run's settings", r.stderr)
        self.assertIn("job stop", r.stderr)

    def test_what_if_changes_nothing(self):
        r = self.run_script("--step", "register-apps", "--what-if")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("BRIDGE_STEP=register-apps", r.stdout)
        self.assertFalse([c for c in self.calls() if "job update" in c or "job start" in c])

    def test_wait_reports_a_failed_execution(self):
        r = self.run_script("--step", "register-apps", "--wait-mins", "5", STUB_EXEC_STATUS="Failed")
        self.assertEqual(r.returncode, 1)
        self.assertIn("ended Failed", r.stderr)
        r = self.run_script("--step", "register-apps", "--wait-mins", "5")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "usnm-boinc-bridge-caj-prod-abc12\n")

    def test_handoff_target_sets_handoff_step_in_the_main_group(self):
        base = {"AZURE_CLIENT_ID": "client-2", "HANDOFF_URL": "https://x.blob.core.windows.net/boinc-handoff",
                "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/", "CURATED_URL": "https://d/curated",
                "REFERENCE_URL": "https://d/reference", "OUT_URL": "https://x/ocr-out"}
        tmpl = json.dumps({"containers": [{"name": "handoff", "env": [{"name": k, "value": v} for k, v in base.items()]}]})
        settings_home = os.path.join(self.tmp, "settings")
        for env_name in ("prod", "dev"):
            env_dir = os.path.join(settings_home, env_name)
            os.makedirs(env_dir)
            with open(os.path.join(env_dir, ".env"), "w") as f:
                f.write(f'AZURE_SUBSCRIPTION_ID="{SUB}"\n')
                f.write(f'AZURE_RESOURCE_GROUP="rg-usnm-{env_name}"\n')
                f.write(f'BOINC_HANDOFF_JOB="usnm-boinc-handoff-caj-{env_name}"\n')
            r = self.run_script("--target", "handoff", "--step", "collect", "--var", "JOB_ID=" + "a" * 64,
                                env_name=env_name, STUB_TEMPLATE=tmpl, STUB_SUB=SUB,
                                USNM_SETTINGS_HOME=settings_home)
            self.assertEqual(r.returncode, 0, r.stderr)
            update = [c for c in self.calls() if c.startswith("containerapp job update")][-1]
            self.assertIn(f"--name usnm-boinc-handoff-caj-{env_name} --resource-group rg-usnm-{env_name}", update)
            self.assertIn("HANDOFF_STEP=collect", update)
            self.assertNotIn("BRIDGE_STEP", update)
            # A job deployed before the queue steps has no QUEUE_URL and still runs.
            self.assertNotIn("QUEUE_URL", update)
        with_queue = json.dumps({"containers": [{"name": "handoff", "env": [
            {"name": k, "value": v} for k, v in {**base, "QUEUE_URL": "https://x.queue.core.windows.net"}.items()]}]})
        r = self.run_script("--target", "handoff", "--step", "claim", "--var", "CLAIM_NONCE=abcdefgh12",
                            env_name="prod", STUB_TEMPLATE=with_queue, STUB_SUB=SUB,
                            USNM_SETTINGS_HOME=settings_home)
        self.assertEqual(r.returncode, 0, r.stderr)
        update = [c for c in self.calls() if c.startswith("containerapp job update")][-1]
        self.assertIn("QUEUE_URL=https://x.queue.core.windows.net", update)
        self.assertIn("HANDOFF_STEP=claim", update)
        r = self.run_script("--target", "handoff", "--step", "claim", "--var", "QUEUE_URL=https://evil",
                            env_name="prod", STUB_TEMPLATE=with_queue, USNM_SETTINGS_HOME=settings_home)
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't be changed per run", r.stderr)
        r = self.run_script("--target", "handoff", "--step", "collect", "--var", "HANDOFF_STEP=x",
                            STUB_TEMPLATE=tmpl, USNM_SETTINGS_HOME=settings_home)
        self.assertEqual(r.returncode, 1)


class BaseVarsMatchBicep(unittest.TestCase):
    def test_lists_agree(self):
        with open(SCRIPT) as f:
            m = re.search(r"^BASE_VARS=\(([^)]*)\)", f.read(), re.M)
        script = m.group(1).split()
        with open(os.path.join(ROOT, "contrib-compute/infra/boinc/modules/bridge-job.bicep")) as f:
            bicep = re.findall(r"\{ name: '([A-Z_]+)', value:", f.read())
        self.assertEqual(sorted(script), sorted(bicep))

    def test_handoff_lists_agree(self):
        with open(SCRIPT) as f:
            text = f.read()
        script = re.search(r"^HANDOFF_BASE_VARS=\(([^)]*)\)", text, re.M).group(1).split()
        script += re.search(r"^HANDOFF_OPTIONAL_VARS=\(([^)]*)\)", text, re.M).group(1).split()
        with open(os.path.join(ROOT, "infra/modules/boinc-handoff-job.bicep")) as f:
            bicep = re.findall(r"\{ name: '([A-Z_]+)', value:", f.read())
        self.assertEqual(sorted(script), sorted(bicep))


TEST_CLIENT_SCRIPT = os.path.join(HERE, "boinc-test-client.sh")
APP_RELEASE_SCRIPT = os.path.join(HERE, "boinc-app-release.sh")
HANDOFF_JOB_SCRIPT = os.path.join(ROOT, "contrib-compute/images/bridge/handoff-job.sh")
BRIDGE_JOB_SCRIPT = os.path.join(ROOT, "contrib-compute/images/server/bridge-job.sh")

STUB_TEST_CLIENT_AZ = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$*" in
  "account show"*) echo "$STUB_SUB" ;;
  "group show"*) ;;
  "stack group show"*)
    if [ -n "${STUB_NO_VAULT:-}" ]; then echo '{}'
    else echo '{"keyVaultName": {"type": "String", "value": "usnmboinckvprod"}}'; fi ;;
  "group create"*) ;;
  "group exists"*) echo "${STUB_GROUP_EXISTS:-true}" ;;
  "group delete"*) ;;
  "deployment group create"*)
    for arg in "$@"; do
      case "$arg" in
        @*) cp "${arg#@}" "$STUB_PARAMS_OUT" ;;
      esac
    done ;;
  *) echo "stub az: unexpected call: $*" >&2; exit 9 ;;
esac
"""


class TestClient(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        with open(os.path.join(bin_dir, "az"), "w") as f:
            f.write(STUB_TEST_CLIENT_AZ)
        os.chmod(os.path.join(bin_dir, "az"), 0o755)
        self.log = os.path.join(self.tmp, "az.log")
        self.params_out = os.path.join(self.tmp, "params.json")
        self.env = {
            "PATH": bin_dir + os.pathsep + os.environ["PATH"],
            "HOME": self.tmp,
            "TMPDIR": self.tmp,
            "STUB_LOG": self.log,
            "STUB_SUB": SUB,
            "STUB_PARAMS_OUT": self.params_out,
        }

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_script(self, *args, **extra):
        return subprocess.run(
            [TEST_CLIENT_SCRIPT, "--env", "prod", *args],
            cwd=ROOT,
            env={**self.env, **extra},
            capture_output=True,
            text=True,
        )

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return f.read().splitlines()

    def test_create_defaults_to_disposable_account_and_deploys_test_group(self):
        r = self.run_script("--create")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        self.assertTrue(any(c.startswith("group create --name usnm-boinc-test-rg-prod") for c in calls))
        self.assertTrue(any(c.startswith("deployment group create --name usnm-boinc-test-prod --resource-group usnm-boinc-test-rg-prod") for c in calls))
        with open(self.params_out) as f:
            params = json.load(f)["parameters"]
        self.assertEqual(params["env"]["value"], "prod")
        self.assertEqual(params["vmSize"]["value"], "Standard_NC4as_T4_v3")
        self.assertIs(params["spot"]["value"], True)
        self.assertEqual(params["projectUrl"]["value"], "https://boinc.usnewsmap.com/usnmocr/")
        self.assertEqual(params["projectAuthenticator"]["value"], "")

    def test_create_supports_paygo_custom_sku_and_authenticator_file(self):
        auth_file = os.path.join(self.tmp, "auth.txt")
        with open(auth_file, "w") as f:
            f.write("0123456789abcdef0123456789abcdef\n")
        r = self.run_script("--create", "--paygo", "--sku", "Standard_D4s_v5", "--authenticator-file", auth_file)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(self.params_out) as f:
            params = json.load(f)["parameters"]
        self.assertEqual(params["vmSize"]["value"], "Standard_D4s_v5")
        self.assertIs(params["spot"]["value"], False)
        self.assertEqual(params["projectAuthenticator"]["value"], "0123456789abcdef0123456789abcdef")

    def test_create_what_if_does_not_create_group_or_deployment(self):
        r = self.run_script("--create", "--what-if")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("would create usnm-boinc-test-rg-prod", r.stdout)
        self.assertFalse([c for c in self.calls() if c.startswith("group create") or c.startswith("deployment group create")])

    def test_create_fails_when_stack_or_authenticator_file_is_invalid(self):
        r = self.run_script("--create", STUB_NO_VAULT="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("has no keyVaultName output", r.stderr)
        self.assertFalse([c for c in self.calls() if c.startswith("group create")])

        empty_auth = os.path.join(self.tmp, "empty.txt")
        open(empty_auth, "w").close()
        r = self.run_script("--create", "--authenticator-file", empty_auth)
        self.assertEqual(r.returncode, 1)
        self.assertIn("must contain a non-empty BOINC authenticator", r.stderr)

    def test_delete_removes_group_or_respects_what_if(self):
        r = self.run_script("--delete", "--what-if")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("would delete resource group usnm-boinc-test-rg-prod", r.stdout)
        self.assertFalse([c for c in self.calls() if c.startswith("group delete")])

        r = self.run_script("--delete")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(any(c.startswith("group delete --name usnm-boinc-test-rg-prod --yes --no-wait") for c in self.calls()))

    def test_rejects_invalid_arguments(self):
        for bad in (["--create", "--delete"], [], ["--create", "--sku", "bad/sku"], ["--create", "--authenticator-file", "/nonexistent/auth"]):
            r = self.run_script(*bad)
            self.assertNotEqual(r.returncode, 0, bad)


class AppReleaseArgs(unittest.TestCase):
    def run_release(self, *args):
        return subprocess.run(
            [APP_RELEASE_SCRIPT, "--env", "prod", "--version", "1.0", "--signing-host", "bad host!", *args],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )

    def test_allows_quality_and_qwen_plan_classes(self):
        img = "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64
        r = self.run_release("--app", "usnm_quality", "--plan-class", "docker")
        self.assertIn("--signing-host must be a host name", r.stderr)

        for pc in ("docker_mt_2b", "docker_nvidia_cuda_8b"):
            r = self.run_release("--app", "usnm_qwen", "--plan-class", pc, "--worker-image", img)
            self.assertIn("--signing-host must be a host name", r.stderr)
        r = self.run_release("--app", "usnm_qwen_train", "--plan-class", "docker_nvidia_train", "--worker-image", img)
        self.assertIn("--signing-host must be a host name", r.stderr)

    def test_prepare_only_is_recognized_before_signing_host_validation(self):
        r = self.run_release("--app", "usnm_ocr", "--prepare-only", "--what-if")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--signing-host must be a host name", r.stderr)

    def test_rejects_qwen_without_worker_image_or_wrong_plan_class(self):
        r = self.run_release("--app", "usnm_qwen", "--plan-class", "docker_nvidia_cuda_8b")
        self.assertEqual(r.returncode, 1)
        self.assertIn("usnm_qwen has its own worker image: give --worker-image", r.stderr)

        r = self.run_release("--app", "usnm_qwen", "--plan-class", "docker")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--app and --plan-class", r.stderr)

        r = self.run_release("--app", "usnm_quality", "--plan-class", "docker_mt")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--app and --plan-class", r.stderr)


class AppReleasePublication(unittest.TestCase):
    def test_prepare_and_publish_paths_update_only_the_appver_pin(self):
        text = Path(APP_RELEASE_SCRIPT).read_text()
        _, marker, tail = text.partition('echo "   $appver_image"\n')
        self.assertTrue(marker)
        old = {"images": {"appver": "old", "worker": "unchanged"}, "assessment": True}
        image = "registry.example.azurecr.io/usnm-boinc-appver@sha256:" + "a" * 64
        for prepare, provision_rc in (("true", "0"), ("false", "0"), ("false", "9")):
            with self.subTest(prepare=prepare, provision_rc=provision_rc), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "scripts").mkdir()
                (root / "build").mkdir()
                settings = root / "settings.json"
                settings.write_text(json.dumps(old))
                log = root / "provision.log"
                provision = root / "scripts/provision-boinc.sh"
                provision.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$*" > "$STUB_LOG"\nexit "$STUB_PROVISION_RC"\n')
                provision.chmod(0o755)
                env = dict(os.environ, PREPARE_ONLY=prepare, STUB_PROVISION_RC=provision_rc,
                           STUB_LOG=str(log), ENV_NAME="prod", appver_image=image,
                           BOINC_SETTINGS=str(settings), work=str(root / "build"),
                           APP="usnm_ocr", VERSION="1.2", PLATFORM="arm64-apple-darwin",
                           PLAN_CLASS="docker")
                result = subprocess.run(["bash", "-c", "set -euo pipefail\n" + tail],
                                        cwd=root, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, int(provision_rc))
                self.assertEqual(log.exists(), prepare == "false")
                if provision_rc != "0":
                    self.assertEqual(json.loads(settings.read_text()), old)
                    self.assertNotIn("published and verified", result.stdout)
                    continue
                expected = dict(old, images=dict(old["images"], appver=image))
                self.assertEqual(json.loads(settings.read_text()), expected)
                if prepare == "true":
                    self.assertIn("not published or registered", result.stdout)
                    self.assertNotIn("release registration and public health verified", result.stdout)
                else:
                    self.assertEqual(log.read_text().strip(), f"--env prod --publish-image {image}")
                    self.assertIn("published and verified", result.stdout)


class HandoffAndBridgeQualitySteps(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        self.log = os.path.join(self.tmp, "usnm-boinc.log")
        stub = os.path.join(bin_dir, "usnm-boinc")
        with open(stub, "w") as f:
            f.write(
                '#!/usr/bin/env bash\n'
                'printf "%s\\n" "$*" >> "$STUB_LOG"\n'
                'while [ $# -gt 0 ]; do\n'
                '  if [ "$1" = "--to" ]; then\n'
                '    mkdir -p "$2" && : > "$2/sample.jsonl"\n'
                '    shift 2\n'
                '  elif [ "$1" = "--summary" ]; then\n'
                '    mkdir -p "$(dirname "$2")" && : > "$2"\n'
                '    shift 2\n'
                '  else\n'
                '    shift\n'
                '  fi\n'
                'done\n'
            )
        os.chmod(stub, 0o755)
        self.env = {
            "PATH": bin_dir + os.pathsep + os.environ["PATH"],
            "TMPDIR": self.tmp,
            "STUB_LOG": self.log,
        }

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return f.read().splitlines()

    def test_handoff_texts_export(self):
        env = {
            **self.env,
            "HANDOFF_STEP": "texts-export",
            "HANDOFF_URL": "https://usnmst.blob.core.windows.net/boinc-handoff",
            "CURATED_URL": "https://usnmst.blob.core.windows.net/curated",
            "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/",
            "REFERENCE_URL": "https://usnmst.blob.core.windows.net/reference",
            "OUT_URL": "https://usnmst.blob.core.windows.net/ocr-out",
            "HANDOFF_PAGES": "sn83045462_1890-05-03_ed-1_seq-1,sn83045462_1890-05-03_ed-1_seq-2",
            "HANDOFF_SAMPLE": "sample.jsonl",
        }
        r = subprocess.run([HANDOFF_JOB_SCRIPT], cwd=ROOT, env=env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 3)
        self.assertIn("handoff get --from-url https://usnmst.blob.core.windows.net/boinc-handoff --prefix quality-samples", calls[0])
        self.assertIn("quality texts-export --curated-url https://usnmst.blob.core.windows.net/curated", calls[1])
        self.assertIn("--doc-id sn83045462_1890-05-03_ed-1_seq-1 --doc-id sn83045462_1890-05-03_ed-1_seq-2", calls[1])
        self.assertIn("sample.jsonl", calls[1])
        self.assertIn("handoff put", calls[2])
        self.assertIn("--prefix texts", calls[2])

    def handoff(self, step, **extra):
        env = {
            **self.env,
            "HANDOFF_STEP": step,
            "HANDOFF_URL": "https://usnmst.blob.core.windows.net/boinc-handoff",
            "CURATED_URL": "https://usnmst.blob.core.windows.net/curated",
            "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/",
            "REFERENCE_URL": "https://usnmst.blob.core.windows.net/reference",
            "OUT_URL": "https://usnmst.blob.core.windows.net/ocr-out",
            **extra,
        }
        return subprocess.run([HANDOFF_JOB_SCRIPT], cwd=ROOT, env=env, capture_output=True, text=True)

    def stub(self, body):
        """Replace the usnm-boinc stub: log every call, then run `body` (bash)."""
        path = os.path.join(self.tmp, "bin", "usnm-boinc")
        with open(path, "w") as f:
            f.write('#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "$STUB_LOG"\n' + body + "\nexit 0\n")
        os.chmod(path, 0o755)

    # Creates what `handoff get` and `get-file` would have copied down.
    COPY_STUB = (
        'to=""; for ((i = 1; i <= $#; i++)); do [ "${!i}" = --to ] && j=$((i + 1)) && to=${!j}; done\n'
        'case "$1 $2" in\n'
        '  "handoff get-file") mkdir -p "$(dirname "$to")" && : > "$to" ;;\n'
        '  "handoff get") mkdir -p "$to/pages-v1" && : > "$to/pages-v1/manifest.parquet" ;;\n'
        'esac'
    )

    def test_handoff_claim_takes_pages_and_writes_claims_before_the_summary(self):
        r = self.handoff(
            "claim",
            QUEUE_URL="https://usnmst.queue.core.windows.net",
            HANDOFF_MANIFEST="pages-v1/manifest.parquet",
            CLAIM_NONCE="abcdefgh12",
            CLAIM_MAX="7",
            CLAIM_QUEUES="ocr-bulk,ocr-priority",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 3, calls)
        self.assertTrue(calls[0].startswith("claim pull --queue-url https://usnmst.queue.core.windows.net "))
        self.assertIn("--manifest-version pages-v1", calls[0])
        self.assertIn("--max 7 --lease-hours 96", calls[0])
        self.assertIn("--nonce abcdefgh12 --queue ocr-bulk --queue ocr-priority", calls[0])
        # The pop receipts exist only in the claim files: they go up first.
        self.assertIn("handoff put", calls[1])
        self.assertIn("--prefix claims", calls[1])
        self.assertIn("--prefix scheduler", calls[2])

    def test_handoff_claim_refuses_bad_settings(self):
        base = dict(QUEUE_URL="https://usnmst.queue.core.windows.net",
                    HANDOFF_MANIFEST="pages-v1/manifest.parquet", CLAIM_NONCE="abcdefgh12")
        for bad in (dict(QUEUE_URL="http://x"), dict(CLAIM_MAX="101"), dict(CLAIM_MAX="0"),
                    dict(CLAIM_QUEUES="ocr-deadletter"), dict(CLAIM_NONCE="short"),
                    dict(CLAIM_LEASE_HOURS="169"), dict(HANDOFF_MANIFEST="../manifest.parquet")):
            r = self.handoff("claim", **{**base, **bad})
            self.assertNotEqual(r.returncode, 0, bad)
        self.assertEqual(self.calls(), [])
        r = self.handoff("claim", HANDOFF_MANIFEST=base["HANDOFF_MANIFEST"], CLAIM_NONCE="abcdefgh12")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("QUEUE_URL is not set", r.stderr)

    def test_handoff_claim_still_uploads_claims_when_the_pull_fails(self):
        # The pull fails after taking messages: their claim files still go up, because the
        # messages are hidden now and their pop receipts exist nowhere else.
        self.stub('[ "$1 $2" = "claim pull" ] && exit 3')
        r = self.handoff(
            "claim",
            QUEUE_URL="https://usnmst.queue.core.windows.net",
            HANDOFF_MANIFEST="pages-v1/manifest.parquet",
            CLAIM_NONCE="abcdefgh12",
        )
        self.assertEqual(r.returncode, 3, r.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2, calls)
        self.assertIn("--prefix claims", calls[1])

    def test_handoff_collect_deletes_the_queue_message_after_the_journal_is_written_back(self):
        job = "a" * 64
        self.stub(
            self.COPY_STUB + "\n"
            'if [ "$1 $2" = "handoff get-spool" ]; then\n'
            '  for ((i = 1; i <= $#; i++)); do [ "${!i}" = --trust-out ] && j=$((i + 1)) && : > "${!j}"; done\n'
            "  echo 17\n"
            "fi"
        )
        r = self.handoff("collect", JOB_ID=job, QUEUE_URL="https://usnmst.queue.core.windows.net")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        (collect,) = [i for i, c in enumerate(calls) if c.startswith("collect ")]
        (finish,) = [i for i, c in enumerate(calls) if c.startswith("claim finish")]
        puts = [i for i, c in enumerate(calls) if c.startswith("handoff put")]
        self.assertTrue(puts and max(puts) < finish and collect < min(puts), calls)
        self.assertIn(f"--job {job}", calls[finish])
        self.assertIn("--queue-url https://usnmst.queue.core.windows.net", calls[finish])

    def test_handoff_collect_without_a_queue_url_or_after_a_failure_deletes_nothing(self):
        job = "a" * 64
        body = (
            self.COPY_STUB + "\n"
            'if [ "$1 $2" = "handoff get-spool" ]; then\n'
            '  for ((i = 1; i <= $#; i++)); do [ "${!i}" = --trust-out ] && j=$((i + 1)) && : > "${!j}"; done\n'
            "  echo 17\n"
            "fi\n"
        )
        self.stub(body)
        r = self.handoff("collect", JOB_ID=job)  # a job deployed before the queue steps
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse([c for c in self.calls() if c.startswith("claim finish")])
        os.remove(self.log)
        self.stub(body + '[ "$1" = collect ] && exit 4')
        r = self.handoff("collect", JOB_ID=job, QUEUE_URL="https://usnmst.queue.core.windows.net")
        self.assertEqual(r.returncode, 4)
        self.assertFalse([c for c in self.calls() if c.startswith("claim finish")])

    def test_handoff_release(self):
        r = self.handoff(
            "release",
            QUEUE_URL="https://usnmst.queue.core.windows.net",
            HANDOFF_DOCS="sn83045462_1890-05-03_ed-1_seq-1,sn83045462_1890-05-03_ed-1_seq-2",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        (call,) = self.calls()
        self.assertIn("claim release --handoff-url https://usnmst.blob.core.windows.net/boinc-handoff", call)
        self.assertIn("--doc-id sn83045462_1890-05-03_ed-1_seq-1 --doc-id sn83045462_1890-05-03_ed-1_seq-2", call)
        r = self.handoff("release", QUEUE_URL="https://usnmst.queue.core.windows.net", HANDOFF_DOCS="a b")
        self.assertNotEqual(r.returncode, 0)

    def test_handoff_export_with_claims_downloads_each_claim_first(self):
        self.stub(self.COPY_STUB)
        r = self.handoff(
            "export",
            HANDOFF_MANIFEST="pages-v1/manifest.parquet",
            HANDOFF_PAGES="sn83045462/1890-05-03/ed-1/seq-1,sn83045462/1890-05-03/ed-1/seq-2",
            HANDOFF_BATCH="dlc_abc",
            HANDOFF_PLAN="queue-feed",
            ENGINE_IMAGE="sha256:" + "a" * 64,
            HANDOFF_CLAIMS="1",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        claim_gets = [c for c in calls if "--path claims/" in c]
        self.assertEqual(len(claim_gets), 2, calls)
        self.assertIn("--path claims/sn83045462_1890-05-03_ed-1_seq-1.json", claim_gets[0])
        export = [c for c in calls if c.startswith("export ")]
        self.assertEqual(len(export), 1)
        self.assertIn("--claims ", export[0])
        self.assertLess(calls.index(claim_gets[1]), calls.index(export[0]))

    def test_handoff_export_without_claims_is_unchanged(self):
        self.stub(self.COPY_STUB)
        r = self.handoff(
            "export",
            HANDOFF_MANIFEST="pages-v1/manifest.parquet",
            HANDOFF_PAGES="sn83045462/1890-05-03/ed-1/seq-1",
            HANDOFF_BATCH="dlc_abc",
            HANDOFF_PLAN="pilot",
            ENGINE_IMAGE="sha256:" + "a" * 64,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        export = [c for c in self.calls() if c.startswith("export ")]
        self.assertEqual(len(export), 1)
        self.assertNotIn("--claims", export[0])

    def test_bridge_quality_and_qwen_steps(self):
        share = os.path.join(self.tmp, "share")
        proj = os.path.join(share, "usnmocr")
        os.makedirs(proj)
        open(os.path.join(proj, ".usnm-initialised"), "w").close()
        bridge_copy = os.path.join(self.tmp, "bridge-job.sh")
        lib_path = os.path.join(ROOT, "contrib-compute/images/server/lib.sh")
        with open(BRIDGE_JOB_SCRIPT) as src, open(bridge_copy, "w") as dst:
            dst.write(src.read().replace("/usr/local/lib/usnm-boinc/lib.sh", lib_path))
        os.chmod(bridge_copy, 0o755)
        base = {
            **self.env,
            "KEY_VAULT_NAME": "usnmboinckvprod",
            "DB_HOST": "db.example.org",
            "DB_NAME": "usnmocr",
            "MASTER_HOST": "boinc.usnewsmap.com",
            "PROJECT_DIR": share,
            "QUALITY_CAMPAIGN": "pilot-1",
            "QWEN_CAMPAIGN": "pilot-1",
            "QUALITY_ENGINE_IMAGE": "sha256:" + "0" * 64,
            "QWEN_ENGINE_IMAGE": "sha256:" + "1" * 64,
            "QWEN_ENGINE_LLAMA": "sha256:" + "2" * 64,
            "QWEN_ENGINE_WEIGHTS": "sha256:" + "3" * 64,
        }
        r = subprocess.run(
            [bridge_copy],
            cwd=ROOT,
            env={
                **base,
                "BRIDGE_STEP": "quality-stage",
                "HANDOFF_URL": "https://usnmst.blob.core.windows.net/boinc-handoff",
                "QUALITY_LANGS": "eng,deu",
                "QUALITY_PAGES": "probe:eng:sn83045462_1890-05-03_ed-1_seq-1:dlc_abc_ver01",
            },
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        self.assertTrue(any("handoff get --from-url https://usnmst.blob.core.windows.net/boinc-handoff --prefix texts" in c for c in calls))
        self.assertTrue(any("quality function-words --lang eng --lang deu" in c for c in calls))
        self.assertTrue(any("quality stage --page probe:eng:sn83045462_1890-05-03_ed-1_seq-1:dlc_abc_ver01" in c for c in calls))

        r = subprocess.run(
            [bridge_copy],
            cwd=ROOT,
            env={**base, "BRIDGE_STEP": "qwen-stage"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(any("qwen stage --quality-root" in c for c in self.calls()))

        for step, expected in [
            ("qwen-benchmark-stage", "qwen benchmark-stage"),
            ("qwen-benchmark-collect", "qwen benchmark-collect"),
        ]:
            r = subprocess.run(
                [bridge_copy], cwd=ROOT,
                env={**base, "BRIDGE_STEP": step, "QWEN_SPOOL": "/spool/v"},
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertTrue(any(expected in c for c in self.calls()))
        self.assertTrue(any("--crops " + share + "/qwen/pilot-1/test/crops.jsonl" in c for c in self.calls()))
        self.assertTrue(any("--out " + share + "/qwen/pilot-1/out" in c for c in self.calls()))

        python_stub = os.path.join(self.tmp, "bin", "python3")
        with open(python_stub, "w") as dst:
            dst.write('#!/usr/bin/env bash\nprintf "prepare %s\\n" "$*" >> "$STUB_LOG"\nexit "${PREP_FAIL:-0}"\n')
        os.chmod(python_stub, 0o755)
        for fail in ("0", "1"):
            before = len(self.calls())
            r = subprocess.run(
                [bridge_copy], cwd=ROOT,
                env={**base, "BRIDGE_STEP": "qwen-benchmark-prepare", "PREP_FAIL": fail},
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, int(fail), r.stderr)
            calls = self.calls()[before:]
            self.assertIn("--selection /usr/local/lib/usnm-boinc/gold/selections/transcription-audited-20261015.json", calls[0])
            self.assertIn("--cache " + share + "/gold/build-v1/out", calls[0])
            if fail == "0":
                self.assertIn("qwen benchmark-stage", calls[1])
                self.assertNotIn("submit", " ".join(calls))
            else:
                self.assertEqual(len(calls), 1)
        r = subprocess.run(
            [bridge_copy], cwd=ROOT,
            env={**base, "BRIDGE_STEP": "qwen-benchmark-prepare", "QWEN_SELECTION": "../other"},
            capture_output=True, text=True,
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("invalid reviewed selection", r.stderr)

        r = subprocess.run(
            [bridge_copy],
            cwd=ROOT,
            env={**base, "BRIDGE_STEP": "quality-collect", "QUALITY_SPOOL": "/spool/q", "QWEN_SPOOL": "/spool/v"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(any("quality collect --campaign pilot-1" in c for c in self.calls()))


PVE_BOINC_CT_SCRIPT = os.path.join(HERE, "boinc/pve-boinc-ct.sh")


class BoincLockAndPveClient(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        with open(os.path.join(bin_dir, "az"), "w") as f:
            f.write(STUB_AZ)
        os.chmod(os.path.join(bin_dir, "az"), 0o755)
        self.log = os.path.join(self.tmp, "az.log")
        self.lock_dir = os.path.join(self.tmp, "boinc-prod.lock")
        self.env = {
            "PATH": bin_dir + os.pathsep + os.environ["PATH"],
            "HOME": self.tmp,
            "STUB_LOG": self.log,
            "STUB_SUB": SUB,
            "STUB_TEMPLATE": template(),
            "USNM_BOINC_LOCK_DIR": self.lock_dir,
        }

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_refuses_when_live_pid_holds_lock_and_allows_reentrant_owner(self):
        os.mkdir(self.lock_dir)
        with open(os.path.join(self.lock_dir, "pid"), "w") as f:
            f.write(f"{os.getpid()}\n")
        with open(os.path.join(self.lock_dir, "cmd"), "w") as f:
            f.write("provision-boinc.sh --env prod\n")

        r = subprocess.run(
            [SCRIPT, "--env", "prod", "--step", "register-apps"],
            cwd=ROOT,
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 1)
        self.assertIn("another BOINC operation is already holding", r.stderr)

        r_reentrant = subprocess.run(
            [SCRIPT, "--env", "prod", "--step", "register-apps"],
            cwd=ROOT,
            env={**self.env, "USNM_BOINC_LOCK_OWNER": str(os.getpid())},
            capture_output=True,
            text=True,
        )
        self.assertEqual(r_reentrant.returncode, 0, r_reentrant.stderr)
        self.assertTrue(os.path.isdir(self.lock_dir))

    def test_recovers_stale_dead_pid_lock_and_releases_on_exit(self):
        os.mkdir(self.lock_dir)
        with open(os.path.join(self.lock_dir, "pid"), "w") as f:
            f.write("999999\n")
        r = subprocess.run(
            [SCRIPT, "--env", "prod", "--step", "register-apps"],
            cwd=ROOT,
            env=self.env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(self.lock_dir))

    def test_pve_boinc_ct_what_if_and_validation(self):
        r = subprocess.run(
            [PVE_BOINC_CT_SCRIPT, "--ctid", "101", "--name", "boinc-volunteer-2", "--what-if"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("pct create 101", r.stdout)
        self.assertIn("--features nesting=1,keyctl=1", r.stdout)

        for bad in (
            ["--ctid", "50", "--what-if"],
            ["--name", "bad_name", "--what-if"],
            ["--max-ncpus-pct", "120", "--what-if"],
            ["--project-url", "http://insecure.example.org/", "--what-if"],
        ):
            res = subprocess.run([PVE_BOINC_CT_SCRIPT, *bad], cwd=ROOT, capture_output=True, text=True)
            self.assertNotEqual(res.returncode, 0, bad)


QUALITY_CAMPAIGN_SCRIPT = os.path.join(HERE, "boinc-quality-campaign.py")


class QualityCampaignCli(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_prepare_splits_manifest_into_round_robin_shards(self):
        manifest = Path(self.tmp, "sample.jsonl")
        rows = [{"doc_id": f"sn83045081_1900-01-{i:02d}_ed-1_seq-1", "lang": "deu"} for i in range(1, 11)]
        manifest.write_text("".join(json.dumps(r) + "\n" for r in rows))
        out_dir = Path(self.tmp, "out")
        res = subprocess.run(
            [
                "python3",
                QUALITY_CAMPAIGN_SCRIPT,
                "prepare",
                "--campaign",
                "v1-test",
                "--manifest",
                str(manifest),
                "--out-dir",
                str(out_dir),
                "--count",
                "8",
                "--shards",
                "4",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("Prepared 8 pages across 4 shards", res.stdout)
        for s in range(4):
            self.assertTrue((out_dir / "shards" / f"shard-s{s}.tar.gz").is_file())


if __name__ == "__main__":
    unittest.main()
