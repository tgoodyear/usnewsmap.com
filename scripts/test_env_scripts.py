"""Tests for the dev lifecycle pieces of scripts/lib/env.sh and for
scripts/dev-status.sh (#275), against a stub `az` (python3 -m unittest
scripts/test_env_scripts.py). Nothing here reaches Azure.

The stub answers from environment variables: STUB_STACK (the stack's
resource count; unset: not found), STUB_GROUPS (group names, one per line),
STUB_TAGS (the project group's "experiment<TAB>started"; unset: no group),
STUB_PARAMS (the guard-rail stack's parameters as two tsv rows) and
STUB_COST (Cost Management's answer; STUB_COST_ERROR makes it fail with that
message). For a job execution (scripts/start-job.sh, and
scripts/titles-seed.sh through it): STUB_EXECUTION (the started execution's name) and
STUB_EXEC_STATUS (its status). For the settings vault (#295): STUB_SECRETS
(the secrets' names, one per line), STUB_SECRET_<NAME> (a secret's value,
its hyphens as underscores), STUB_VAULT_ERROR (listing or setting secrets
fails with that message) and STUB_OUTPUTS (the stack's outputs as JSON).
`az account set` selects a subscription (STUB_ACCOUNTS: the ones az is
signed in to, one per line; unset: any), and each vault call is logged with
the selected one to STUB_LOG.vault. It logs each call to STUB_LOG.

The committed settings (infra/env/<env>.json) are read from a directory of
each test's own (USNM_ENV_DIR, a git repository holding a copy of the
schema), never the checkout's."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

STUB_AZ = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2" in
  "account show") case "$*" in *"--query id"*) echo sub-1 ;; esac ;;
  "account set") s=$(sed 's/.*--subscription \([^ ]*\).*/\1/' <<< "$*")
    if [ -n "${STUB_ACCOUNTS:-}" ] && ! printf '%b\n' "$STUB_ACCOUNTS" | grep -qx "$s"; then
      echo "ERROR: The subscription of '$s' doesn't exist in cloud 'AzureCloud'." >&2; exit 1
    fi
    echo "$s" > "$STUB_LOG.account" ;;
  "stack sub")
    case "$*" in
      "stack sub create"*) ;;
      *"--query location"*) echo eastus2 ;;
      *"keys(parameters"*) printf '%s\n' "${STUB_PARAMS:-}" ;;
      *"--query outputs -o json"*)
        if [ -n "${STUB_OUTPUTS:-}" ]; then printf '%s\n' "$STUB_OUTPUTS"
        elif [ -n "${STUB_STACK:-}" ]; then echo '{}'
        else echo "ERROR: DeploymentStackNotFound: usnm-dev could not be found." >&2; exit 3; fi ;;
      *) if [ -n "${STUB_STACK:-}" ]; then echo "$STUB_STACK"
         else echo "ERROR: DeploymentStackNotFound: usnm-dev could not be found." >&2; exit 3; fi ;;
    esac ;;
  "keyvault secret")
    printf '%s %s\n' "$(cat "$STUB_LOG.account" 2> /dev/null || echo none)" "$3" >> "$STUB_LOG.vault"
    case "$3" in
      list) if [ -n "${STUB_VAULT_ERROR:-}" ]; then echo "$STUB_VAULT_ERROR" >&2; exit 1; fi
        [ -z "${STUB_SECRETS:-}" ] || printf '%b\n' "$STUB_SECRETS" ;;
      set) if [ -n "${STUB_VAULT_ERROR:-}" ]; then echo "$STUB_VAULT_ERROR" >&2; exit 1; fi ;;
      show) n=$(sed 's/.*--name \([^ ]*\).*/\1/' <<< "$*"); v="STUB_SECRET_${n//-/_}"
        if [ -n "${!v+x}" ]; then printf '%s\n' "${!v}"
        else echo "ERROR: (SecretNotFound) A secret with (name/id) $n was not found in this key vault." >&2; exit 3; fi ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "group list") [ -z "${STUB_GROUPS:-}" ] || printf '%s\n' "$STUB_GROUPS" ;;
  "group show")
    if [ -n "${STUB_TAGS+x}" ]; then printf '%b\n' "$STUB_TAGS"
    else echo "ERROR: (ResourceGroupNotFound) Resource group 'rg-usnm-dev' could not be found." >&2; exit 3; fi ;;
  "resource list") printf 'Microsoft.Storage/storageAccounts\tstusnmdev\n' ;;
  "containerapp job")
    case "$*" in
      *"execution show"*) echo "${STUB_EXEC_STATUS:-Succeeded}" ;;
      *"job show"*) echo '{"containers":[{"name":"ingest","image":"cr/usnewsmap-ingest:exp-1","args":["run"],"env":[]}]}' ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "rest --method")
    case "$*" in *"/jobs/"*"/start?"*) echo "${STUB_EXECUTION:-}"; exit 0 ;; esac
    if [ -n "${STUB_COST_ERROR:-}" ]; then echo "$STUB_COST_ERROR" >&2; exit 1; fi
    printf '%s\n' "${STUB_COST:-}" ;;
  *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
esac
"""

COST = json.dumps({"properties": {
    "columns": [{"name": "Cost"}, {"name": "ResourceGroupName"}, {"name": "Currency"}],
    "rows": [[1.25, "rg-usnm-dev", "USD"], [0.5, "me_cae-usnm-dev_rg-usnm-dev_eastus2", "USD"],
             [40.0, "rg-usnm-prod", "USD"], [0.1, "rg-usnm-dev2", "USD"]]}})


class Stubbed(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        with open(os.path.join(bin_dir, "az"), "w") as f:
            f.write(STUB_AZ)
        os.chmod(os.path.join(bin_dir, "az"), 0o755)
        # Settings in a directory of the test's own, never a checkout's.
        self.settings = os.path.join(self.tmp, "settings")
        os.makedirs(os.path.join(self.settings, "dev"))
        self.log = os.path.join(self.tmp, "az.log")
        # The committed settings: a repository of the test's own.
        self.env_dir = os.path.join(self.tmp, "env")
        os.mkdir(self.env_dir)
        shutil.copy(os.path.join(ROOT, "infra", "env", "schema.json"), self.env_dir)
        self.env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "HOME": self.tmp,
                    "USNM_SETTINGS_HOME": self.settings, "STUB_LOG": self.log,
                    "USNM_ENV_DIR": self.env_dir,
                    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
        self.git("init", "-q")
        self.git("add", "schema.json")
        self.git("commit", "-q", "-m", "schema")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def git(self, *args):
        subprocess.run(["git", "-C", self.env_dir, "-c", "user.name=t", "-c", "user.email=t@example.org",
                        *args], check=True, env=self.env, capture_output=True)

    def settings_file(self, text, env="dev"):
        os.makedirs(os.path.join(self.settings, env), exist_ok=True)
        with open(os.path.join(self.settings, env, ".env"), "w") as f:
            f.write(text)

    def local_text(self, env="dev"):
        with open(os.path.join(self.settings, env, ".env")) as f:
            return f.read()

    def committed(self, data, env="dev", commit=True):
        """Writes infra/env/<env>.json (and commits it)."""
        data = dict(sorted({"$schema": "./schema.json", **data}.items()))
        with open(os.path.join(self.env_dir, f"{env}.json"), "w") as f:
            f.write(json.dumps(data, indent=2) + "\n")
        if commit:
            self.git("add", f"{env}.json")
            self.git("commit", "-q", "-m", env)

    def committed_data(self, env="dev"):
        with open(os.path.join(self.env_dir, f"{env}.json")) as f:
            return json.load(f)

    def lib(self, script, env_name="dev", **extra):
        """Runs `script` with scripts/lib/env.sh sourced, as the scripts do."""
        prog = f'set -euo pipefail\ndie() {{ echo "error: $*" >&2; exit 1; }}\nENV_NAME={env_name}\n' \
               f'. scripts/lib/env.sh\n{script}\n'
        return subprocess.run(["bash", "-c", prog], cwd=ROOT, env={**self.env, **extra},
                              capture_output=True, text=True)

    def calls(self):
        with open(self.log) as f:
            return f.read().splitlines()


class SettingsLocation(Stubbed):
    def test_the_shared_directory_wins(self):
        r = self.lib('echo "$ENV_FILE"')
        self.assertEqual(r.stdout.strip(), os.path.join(self.settings, "dev", ".env"))

    def test_without_it_the_checkout_holds_them(self):
        r = self.lib('echo "$ENV_FILE"', env_name="nosuchenv")
        self.assertEqual(r.stdout.strip(), ".azure/nosuchenv/.env")


class ReservedNames(Stubbed):
    def test_other_stacks_names_are_reserved(self):
        # usnm-archive and usnm-guardrails are stacks of their own.
        for name in ("archive", "guardrails"):
            r = self.lib("true", env_name=name)
            self.assertEqual(r.returncode, 2, name)
            self.assertIn("reserved", r.stderr)


class ExperimentMarker(Stubbed):
    def test_dev_needs_an_experiment(self):
        self.settings_file('AZURE_ENV_NAME="dev"\n')
        r = self.lib("experiment_marker")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("USNM_EXPERIMENT", r.stderr)

    def test_teardown_needs_none(self):
        self.settings_file('AZURE_ENV_NAME="dev"\n')
        r = self.lib("experiment_marker", NO_EXPERIMENT_CHECK="true")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_teardown_reads_no_marker(self):
        # Not even a malformed one, nor the group's tags.
        self.settings_file('USNM_EXPERIMENT="search"\n')
        r = self.lib("experiment_marker", NO_EXPERIMENT_CHECK="true")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(self.log) and any(c.startswith("group show") for c in self.calls()))

    def test_other_environments_need_none(self):
        r = self.lib("experiment_marker", env_name="prod")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_an_issue_number(self):
        self.settings_file('USNM_EXPERIMENT="search"\n')
        r = self.lib("experiment_marker")
        self.assertNotEqual(r.returncode, 0)

    def test_a_new_group_starts_now(self):
        self.settings_file('USNM_EXPERIMENT="275"\n')
        r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout.strip(), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")

    def test_the_same_experiment_keeps_its_start(self):
        self.settings_file('USNM_EXPERIMENT="275"\n')
        r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED",
                     STUB_TAGS="275\\t2026-10-01T12:00:00Z")
        self.assertEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")

    def test_another_experiment_starts_again(self):
        self.settings_file('USNM_EXPERIMENT="301"\n')
        r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED",
                     STUB_TAGS="275\\t2026-10-01T12:00:00Z")
        self.assertNotEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")
        self.assertRegex(r.stdout.strip(), r"^\d{4}-")


class GuardrailBudget(Stubbed):
    def create_call(self):
        return [c for c in self.calls() if c.startswith("stack sub create")][0]

    def test_a_new_budget_starts_this_month(self):
        self.settings_file('USNM_ALERT_EMAILS="ops@example.org"\n')
        r = self.lib("deploy_env_guardrails")
        self.assertEqual(r.returncode, 0, r.stderr)
        call = self.create_call()
        self.assertIn("devBudgetEmails=ops@example.org", call)
        self.assertRegex(call, r"devBudgetStart=\d{4}-\d\d-01 ")

    def test_an_environment_without_recipients_keeps_the_budget(self):
        self.settings_file('AZURE_ENV_NAME="dev"\n')
        r = self.lib("deploy_env_guardrails",
                     STUB_PARAMS="devbudgetEmails\tDevBudgetStart\nops@example.org\t2026-10-01")
        self.assertEqual(r.returncode, 0, r.stderr)
        call = self.create_call()
        self.assertIn("devBudgetEmails=ops@example.org devBudgetStart=2026-10-01 ", call)

    def test_new_recipients_keep_the_start(self):
        self.settings_file('USNM_ALERT_EMAILS="new@example.org"\n')
        self.lib("deploy_env_guardrails",
                 STUB_PARAMS="devBudgetEmails\tdevBudgetStart\nops@example.org\t2026-10-01")
        self.assertIn("devBudgetEmails=new@example.org devBudgetStart=2026-10-01 ", self.create_call())


    def test_dev_names_its_region(self):
        self.settings_file('AZURE_LOCATION="westus3"\n')
        self.lib("deploy_env_guardrails")
        self.assertIn("devBudgetLocation=westus3", self.create_call())

    def test_other_environments_keep_the_region(self):
        self.settings_file('AZURE_LOCATION="eastus2"\n', env="prod")
        r = self.lib("deploy_env_guardrails", env_name="prod",
                     STUB_PARAMS="devBudgetEmails\tdevBudgetStart\tdevBudgetLocation\n"
                                 "ops@example.org\t2026-10-01\twestus3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("devBudgetLocation=westus3", self.create_call())


    def test_the_archive_keeps_the_budget(self):
        # archive-store.sh deploy passes no recipients and no region.
        r = self.lib("deploy_guardrails eastus2", env_name="prod",
                     STUB_PARAMS="devBudgetEmails\tdevBudgetStart\tdevBudgetLocation\n"
                                 "ops@example.org\t2026-10-01\twestus3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("devBudgetEmails=ops@example.org devBudgetStart=2026-10-01 "
                      "devBudgetLocation=westus3 ", self.create_call())


class DevStatus(Stubbed):
    def status(self, **extra):
        return subprocess.run([os.path.join(HERE, "dev-status.sh"), "--subscription", "sub-1"], cwd=ROOT,
                              env={**self.env, "STUB_COST": COST, **extra}, capture_output=True, text=True)

    def test_nothing_there(self):
        r = self.status()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("dev holds nothing", r.stdout)

    def test_resources_without_a_tag(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev\nrg-usnm-dev-spot", STUB_TAGS="\\t")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no experiment tag", r.stderr)

    def test_a_group_left_behind(self):
        r = self.status(STUB_GROUPS="ME_cae-usnm-dev_rg-usnm-dev_eastus2")
        self.assertEqual(r.returncode, 1)

    def test_an_experiment(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev",
                        STUB_TAGS="275\\t2026-10-01T12:00:00Z")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("experiment #275, started 2026-10-01T12:00:00Z", r.stdout)
        # dev's groups only, whatever their case: not prod's, not dev2's.
        self.assertIn("cost since 2026-10-01T12:00:00Z: 1.75 USD", r.stdout)
        self.assertTrue(re.search(r"group rg-usnm-dev: 1 resources", r.stdout))

    def test_cost_without_the_reader_role_is_a_warning(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\\t2026-10-01T12:00:00Z",
                        STUB_COST_ERROR="ERROR: (AuthorizationFailed) no access")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("needs Cost Management Reader", r.stderr)

    def test_any_other_cost_error_fails(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\\t2026-10-01T12:00:00Z",
                        STUB_COST_ERROR="ERROR: (TooManyRequests) throttled")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the cost", r.stderr)


class StartJobWait(Stubbed):
    """scripts/start-job.sh --wait-mins: the teardown runbook's titles-archive
    step must not return before the copy is written (#275)."""
    SETTINGS = ('AZURE_SUBSCRIPTION_ID="sub-1"\nAZURE_RESOURCE_GROUP="rg-usnm-dev"\n'
                'INGEST_JOB="caj-usnm-ingest-dev"\n')

    def start(self, *args, **extra):
        self.settings_file(self.SETTINGS)
        return subprocess.run([os.path.join(HERE, "start-job.sh"), "--env", "dev", "--job", "INGEST_JOB",
                               *args, "--", "titles-archive"], cwd=ROOT,
                              env={**self.env, "STUB_EXECUTION": "caj-usnm-ingest-dev-xyz34", **extra},
                              capture_output=True, text=True)

    def polled(self):
        return [c for c in self.calls() if "execution show" in c]

    def test_without_it_returns_once_started(self):
        r = self.start(STUB_EXEC_STATUS="Running")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "caj-usnm-ingest-dev-xyz34\n")
        self.assertEqual(self.polled(), [])

    def test_waits_for_success(self):
        r = self.start("--wait-mins", "20")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "caj-usnm-ingest-dev-xyz34\n")
        self.assertEqual(len(self.polled()), 1)
        self.assertIn("--job-execution-name caj-usnm-ingest-dev-xyz34", self.polled()[0])
        body = json.loads([c for c in self.calls() if c.startswith("rest --method post")][0]
                          .split("--body ", 1)[1].split(" --query", 1)[0])
        self.assertEqual(body["containers"][0]["args"], ["titles-archive"])

    def test_a_failed_execution_fails(self):
        for status in ("Failed", "Stopped", "Degraded"):
            r = self.start("--wait-mins", "20", STUB_EXEC_STATUS=status)
            self.assertEqual(r.returncode, 1, status)
            self.assertIn(f"ended {status}", r.stderr)
            self.assertEqual(r.stdout, "")

    def test_still_running_at_the_deadline_fails(self):
        r = self.start("--wait-mins", "00", STUB_EXEC_STATUS="Running")
        self.assertEqual(r.returncode, 1)
        self.assertIn("still Running after 0 minutes", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_no_execution_fails(self):
        r = self.start("--wait-mins", "20", STUB_EXECUTION="")
        self.assertEqual(r.returncode, 1)
        self.assertIn("started no execution", r.stderr)

    def test_wait_mins_is_a_number(self):
        # An empty value (an unset variable) is refused, not read as no wait.
        for value in ("soon", "", "-5"):
            r = self.start("--wait-mins", value)
            self.assertEqual(r.returncode, 2, repr(value))
            self.assertIn("--wait-mins is a number of minutes", r.stderr)
            self.assertFalse(os.path.exists(self.log))


class TitlesSeed(Stubbed):
    SETTINGS = ('AZURE_SUBSCRIPTION_ID="sub-1"\nAZURE_RESOURCE_GROUP="rg-usnm-dev"\n'
                'INGEST_JOB="caj-usnm-ingest-dev"\n'
                'USNM_ARCHIVE_ACCOUNT="/subscriptions/sub-1/resourceGroups/rg-usnm-archive/providers/'
                'Microsoft.Storage/storageAccounts/stusnmarchivex"\n')

    def seed(self, *args, env="dev", **extra):
        return subprocess.run([os.path.join(HERE, "titles-seed.sh"), "--env", env, *args], cwd=ROOT,
                              env={**self.env, "STUB_EXECUTION": "caj-usnm-ingest-dev-abc12", **extra},
                              capture_output=True, text=True)

    def test_runs_titles_seed_in_the_ingest_job_and_waits(self):
        self.settings_file(self.SETTINGS)
        r = self.seed()
        self.assertEqual(r.returncode, 0, r.stderr)
        start = [c for c in self.calls() if c.startswith("rest --method post")]
        self.assertEqual(len(start), 1)
        self.assertIn("/jobs/caj-usnm-ingest-dev/start?", start[0])
        # Only the arguments change: the job's own image and settings stay.
        body = json.loads(start[0].split("--body ", 1)[1].split(" --query", 1)[0])
        self.assertEqual(body["containers"][0]["args"], ["titles-seed"])
        self.assertEqual(body["containers"][0]["image"], "cr/usnewsmap-ingest:exp-1")
        self.assertTrue(any("--job-execution-name caj-usnm-ingest-dev-abc12" in c for c in self.calls()))
        self.assertIn("--query-name titles-archive", r.stdout)
        # Nothing here touches the archival account or its stack.
        self.assertFalse(any(c.startswith(("storage", "stack", "lock", "role")) for c in self.calls()))

    def test_a_failed_execution_fails(self):
        self.settings_file(self.SETTINGS)
        r = self.seed(STUB_EXEC_STATUS="Failed")
        self.assertEqual(r.returncode, 1)
        self.assertIn("ended Failed", r.stderr)

    def test_still_running_at_the_deadline(self):
        self.settings_file(self.SETTINGS)
        r = self.seed("--wait-mins", "0", STUB_EXEC_STATUS="Running")
        self.assertEqual(r.returncode, 1)
        self.assertIn("still Running", r.stderr)

    def test_wait_mins_with_a_leading_zero_is_decimal(self):
        # `08` isn't octal: the wait is 8 minutes, not "value too great for base".
        self.settings_file(self.SETTINGS)
        r = self.seed("--wait-mins", "08")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("base", r.stderr)
        r = self.seed("--wait-mins", "00", STUB_EXEC_STATUS="Running")
        self.assertEqual(r.returncode, 1)
        self.assertIn("still Running after 0 minutes", r.stderr)

    def test_needs_the_archive(self):
        self.settings_file('AZURE_SUBSCRIPTION_ID="sub-1"\nINGEST_JOB="caj-usnm-ingest-dev"\n')
        r = self.seed()
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_ARCHIVE_ACCOUNT", r.stderr)
        self.assertFalse(os.path.exists(self.log))

    def test_needs_the_ingest_job(self):
        self.settings_file(self.SETTINGS.replace('INGEST_JOB="caj-usnm-ingest-dev"\n', ""))
        r = self.seed()
        self.assertEqual(r.returncode, 1)
        self.assertIn("no ingest job", r.stderr)
        self.assertFalse(os.path.exists(self.log))

    def test_never_prod(self):
        r = self.seed(env="prod")
        self.assertEqual(r.returncode, 2)
        self.assertFalse(os.path.exists(self.log))

    def test_named_arguments_only(self):
        r = subprocess.run([os.path.join(HERE, "titles-seed.sh"), "dev"], cwd=ROOT, env=self.env,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)


class CommittedSettings(Stubbed):
    """infra/env/<env>.json, the vault and the local file (#295)."""

    VAULT = {"SETTINGS_KEYVAULT": "usnmcorekvdevabc123"}

    def local(self, **values):
        self.settings_file("".join(f'{k}="{v}"\n' for k, v in values.items()))

    def test_a_public_setting_is_read_from_the_committed_file(self):
        self.committed({"USNM_JA_LATIN": True, "USNM_INGEST_SCRATCH_GIB": 64})
        r = self.lib("aget USNM_JA_LATIN; aget USNM_INGEST_SCRATCH_GIB; aget USNM_OCR")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["true", "64", ""])

    def test_a_public_setting_is_written_to_the_committed_file(self):
        self.local(USNM_SEARCH_BACKEND="fixtures", API_URL="https://api.example.org")
        r = self.lib("aset USNM_SEARCH_BACKEND quickwit; aset USNM_JA_LATIN true; aset USNM_SPLIT_DOCS 120000")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data(), {"$schema": "./schema.json", "USNM_JA_LATIN": True,
                                                 "USNM_SEARCH_BACKEND": "quickwit", "USNM_SPLIT_DOCS": 120000})
        # Out of the local file, where it no longer counts; outputs stay.
        self.assertEqual(self.local_text(), 'API_URL="https://api.example.org"\n')
        # Sorted, in the canonical format.
        r = subprocess.run(["python3", os.path.join(HERE, "lib", "env_settings.py"), "--check"],
                           env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_an_empty_value_unsets_a_public_setting(self):
        self.committed({"USNM_API_IMAGE": "cr/usnewsmap-api:1"})
        r = self.lib("aset USNM_API_IMAGE ''; aset USNM_OCR true; adel USNM_OCR")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data(), {"$schema": "./schema.json"})

    def test_a_value_of_the_wrong_type_is_refused(self):
        r = self.lib("aset USNM_JA_LATIN yes")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("true or false", r.stderr)
        r = self.lib("aset USNM_SPLIT_DOCS 12k")
        self.assertNotEqual(r.returncode, 0)

    def test_private_and_local_settings_stay_local(self):
        r = self.lib("aset USNM_ALERT_EMAILS ops@example.org; aset USNM_EXPERIMENT 295; aset API_URL x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.env_dir, "dev.json")))
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())

    def test_an_old_local_public_setting_is_read_with_a_warning(self):
        self.local(USNM_OCR="true")
        r = self.lib("aget USNM_OCR")
        self.assertEqual(r.stdout.strip(), "true")
        self.assertIn("--migrate", r.stderr)

    def test_a_committed_file_that_doesnt_check_out_stops_everything(self):
        with open(os.path.join(self.env_dir, "dev.json"), "w") as f:
            f.write('{\n  "USNM_ALERT_EMAILS": "ops@example.org"\n}\n')
        r = self.lib("echo ran")
        self.assertEqual(r.returncode, 1)
        self.assertNotIn("ran", r.stdout)
        self.assertIn("private", r.stderr)

    def test_private_settings_come_from_the_vault(self):
        self.local(**self.VAULT)
        r = self.lib("load_vault; aget USNM_ALERT_EMAILS; aget USNM_OCR_USERS",
                     STUB_SECRETS="USNM-ALERT-EMAILS\\nusnm-ocr-users",
                     STUB_SECRET_USNM_ALERT_EMAILS='ops@example.org', STUB_SECRET_usnm_ocr_users='a"b$c')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["ops@example.org", 'a"b$c'])
        self.assertTrue(any(c.startswith("keyvault secret list --vault-name usnmcorekvdevabc123") for c in self.calls()))

    def test_other_scripts_read_one_secret(self):
        # Without load_vault (onboard.sh, group.sh, ...): one secret, by name.
        self.local(**self.VAULT)
        r = self.lib("aget USNM_OCR_NODE_GROUP; aget USNM_OCR_USERS",
                     STUB_SECRET_USNM_OCR_NODE_GROUP="group-1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["group-1", ""])
        self.assertFalse(any(c.startswith("keyvault secret list") for c in self.calls()))

    def test_an_unreadable_vault_fails_the_read(self):
        self.local(SETTINGS_KEYVAULT="usnm-no-such")
        with open(os.path.join(self.tmp, "bin", "az"), "w") as f:
            f.write('#!/usr/bin/env bash\necho "ERROR: (Forbidden) not allowed" >&2\nexit 1\n')
        r = self.lib("aget USNM_OCR_NODE_GROUP")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read USNM_OCR_NODE_GROUP", r.stderr)

    def test_no_vault_yet_reads_nothing(self):
        # No stack, or a stack without the output: no vault.
        for stack in ({}, {"STUB_STACK": "3"}):
            r = self.lib("load_vault; aget USNM_ALERT_EMAILS", **stack)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertFalse(any(c.startswith("keyvault") for c in self.calls()))

    def test_without_a_local_name_the_vault_comes_from_the_stack(self):
        # Another operator's machine: the stack names the vault.
        self.local(API_URL="x")
        r = self.lib("load_vault; aget USNM_ALERT_EMAILS",
                     STUB_OUTPUTS='{"settingS_KEYVAULT": {"type": "String", "value": "usnmcorekvdevabc123"}}',
                     STUB_SECRETS="USNM-ALERT-EMAILS", STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "ops@example.org")
        self.assertIn('SETTINGS_KEYVAULT="usnmcorekvdevabc123"', self.local_text())

    def test_an_operator_without_access_is_told_how_to_get_it(self):
        self.local(**self.VAULT)
        r = self.lib("load_vault", STUB_VAULT_ERROR="ERROR: (Forbidden) Caller is not authorized. ForbiddenByRbac")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_SETTINGS_VAULT_ADMINS", r.stderr)
        self.assertIn("--name USNM-SETTINGS-VAULT-ADMINS", r.stderr)
        self.assertIn('az role assignment create --assignee-object-id "$(az ad signed-in-user show', r.stderr)
        self.assertIn("/resourceGroups/rg-usnm-dev", r.stderr)

    def test_a_vault_secret_that_isnt_a_private_setting_stops(self):
        self.local(**self.VAULT)
        for name in ("USNM-JA-LATIN", "rhel-activation-key"):
            r = self.lib("load_vault", STUB_SECRETS=name)
            self.assertEqual(r.returncode, 1, name)
            self.assertIn("isn't a private setting", r.stderr)

    def test_a_local_entry_shadows_the_vault(self):
        # teardown.sh clears USNM_ARCHIVE_ACCOUNT for its provision.
        self.local(**self.VAULT, USNM_ARCHIVE_ACCOUNT="")
        r = self.lib("load_vault; settings_check; aget USNM_ARCHIVE_ACCOUNT",
                     STUB_SECRETS="USNM-ARCHIVE-ACCOUNT", STUB_SECRET_USNM_ARCHIVE_ACCOUNT="/subscriptions/x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "")

    def test_a_private_setting_in_two_places_stops(self):
        self.local(**self.VAULT, USNM_ALERT_EMAILS="old@example.org")
        r = self.lib("load_vault; settings_check",
                     STUB_SECRETS="USNM-ALERT-EMAILS", STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_ALERT_EMAILS is in both", r.stderr)
        self.assertNotIn("ops@example.org", r.stderr)

    def test_a_private_setting_still_local_is_a_warning_with_the_move(self):
        self.local(**self.VAULT, USNM_ALERT_EMAILS="ops@example.org")
        r = self.lib("load_vault; settings_check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("az keyvault secret set --vault-name usnmcorekvdevabc123 --name USNM-ALERT-EMAILS", r.stderr)
        # The command reads the value; it isn't printed.
        self.assertNotIn("ops@example.org", r.stderr)

    def test_a_public_setting_in_the_local_file_stops(self):
        self.local(USNM_JA_LATIN="true", API_URL="x")
        r = self.lib("settings_check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_JA_LATIN is in", r.stderr)
        self.assertIn("--migrate", r.stderr)

    def test_provision_checks_before_anything_else(self):
        self.settings_file('AZURE_SUBSCRIPTION_ID="sub-1"\nUSNM_JA_LATIN="true"\n', env="prod")
        r = self.lib("provision", env_name="prod")
        self.assertEqual(r.returncode, 1)
        self.assertIn("not provisioning prod", r.stderr)
        self.assertFalse(os.path.exists(self.log) and any(c.startswith("stack sub create") for c in self.calls()))

    def vault_sets(self):
        return [c for c in self.calls() if c.startswith("keyvault secret set")]

    def test_a_script_writes_a_private_setting_to_the_vault_once_it_exists(self):
        # bootstrap.sh and group.sh set them with aset; a copy left local
        # (from before the vault) moves into the vault.
        self.local(**self.VAULT, USNM_GITHUB_REPO_IDS="o@1/r@2")
        r = self.lib("aset USNM_GITHUB_REPO_IDS o@1/r@3; aset USNM_OCR_NODE_GROUP group-1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.vault_sets(), [
            "keyvault secret set --vault-name usnmcorekvdevabc123 --name USNM-GITHUB-REPO-IDS --value o@1/r@3 -o none",
            "keyvault secret set --vault-name usnmcorekvdevabc123 --name USNM-OCR-NODE-GROUP --value group-1 -o none"])
        self.assertEqual(self.local_text(), 'SETTINGS_KEYVAULT="usnmcorekvdevabc123"\n')

    def test_a_bootstrap_rerun_after_migration_leaves_one_copy(self):
        # The values bootstrap.sh sets again are in the vault already.
        self.local(**self.VAULT, API_URL="x")
        vault = {"STUB_SECRETS": "USNM-GITHUB-REPO-IDS\\nUSNM-ALERT-EMAILS\\nUSNM-API-CERT-ID",
                 "STUB_SECRET_USNM_GITHUB_REPO_IDS": "o@1/r@2", "STUB_SECRET_USNM_ALERT_EMAILS": "ops@example.org",
                 "STUB_SECRET_USNM_API_CERT_ID": "/subscriptions/x/certificates/api"}
        r = self.lib("aset USNM_GITHUB_REPO_IDS o@1/r@2; aset USNM_ALERT_EMAILS ops@example.org\n"
                     "aset USNM_API_CERT_ID /subscriptions/x/certificates/api; load_vault; settings_check", **vault)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.vault_sets(), [])
        self.assertEqual(self.local_text(), 'SETTINGS_KEYVAULT="usnmcorekvdevabc123"\nAPI_URL="x"\n')
        self.assertNotIn("warning", r.stderr)
        # After load_vault (provision's): only a changed value is written.
        r = self.lib("load_vault; aset USNM_ALERT_EMAILS ops@example.org; aset USNM_ALERT_EMAILS new@example.org",
                     **vault)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.vault_sets(), ["keyvault secret set --vault-name usnmcorekvdevabc123 "
                                             "--name USNM-ALERT-EMAILS --value new@example.org -o none"])

    def test_an_empty_private_value_stays_local(self):
        # teardown.sh's detach: a local "" shadows the vault.
        self.local(**self.VAULT)
        r = self.lib("aset USNM_ARCHIVE_ACCOUNT ''")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('USNM_ARCHIVE_ACCOUNT=""', self.local_text())
        self.assertFalse(os.path.exists(self.log) and self.vault_sets())

    def test_a_refused_vault_write_fails_with_the_fix(self):
        self.local(**self.VAULT)
        r = self.lib("aset USNM_ALERT_EMAILS ops@example.org",
                     STUB_VAULT_ERROR="ERROR: (Forbidden) Caller is not authorized.")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't set USNM_ALERT_EMAILS", r.stderr)
        self.assertIn("az role assignment create", r.stderr)
        self.assertNotIn("USNM_ALERT_EMAILS", self.local_text())

    def test_a_move_brings_the_old_vaults_settings_back(self):
        # bootstrap.sh --move: the new subscription gets a new vault.
        self.local(**self.VAULT, USNM_OCR_USERS="kept-local", API_URL="x")
        r = self.lib("vault_detach\naset USNM_API_CERT_ID ''; aset USNM_GITHUB_REPO_IDS o@1/r@2\n"
                     "load_vault; settings_check",
                     STUB_SECRETS="USNM-ALERT-EMAILS\\nUSNM-API-CERT-ID\\nUSNM-OCR-USERS",
                     STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org", STUB_SECRET_USNM_OCR_USERS="vault",
                     STUB_SECRET_USNM_API_CERT_ID="/subscriptions/old/certificates/api")
        self.assertEqual(r.returncode, 0, r.stderr)
        text = self.local_text()
        self.assertNotIn("SETTINGS_KEYVAULT", text)
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', text)
        self.assertIn('USNM_OCR_USERS="kept-local"', text)
        self.assertIn('USNM_API_CERT_ID=""', text)
        self.assertIn('USNM_GITHUB_REPO_IDS="o@1/r@2"', text)
        self.assertEqual(self.vault_sets(), [])
        # A rerun once the new vault exists moves what bootstrap sets into it.
        self.settings_file(text + 'SETTINGS_KEYVAULT="usnmcorekvdevnew456"\n')
        r = self.lib("aset USNM_GITHUB_REPO_IDS o@1/r@2; load_vault; settings_check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("USNM_GITHUB_REPO_IDS", self.local_text())
        self.assertEqual(self.vault_sets(), ["keyvault secret set --vault-name usnmcorekvdevnew456 "
                                             "--name USNM-GITHUB-REPO-IDS --value o@1/r@2 -o none"])

    def test_a_move_from_an_unreadable_vault_stops(self):
        self.local(**self.VAULT)
        r = self.lib("vault_detach", STUB_VAULT_ERROR="ERROR: (Forbidden) Caller is not authorized.")
        self.assertEqual(r.returncode, 1)
        self.assertIn("SETTINGS_KEYVAULT", self.local_text())

    def test_a_move_reads_the_old_vault_in_the_old_subscription(self):
        # bootstrap.sh --move into another tenant: the old vault is read
        # with the old subscription selected, then the new one is again.
        self.local(**self.VAULT, AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("vault_detach_from sub-old sub-new", STUB_SECRETS="USNM-ALERT-EMAILS",
                     STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(self.log + ".vault") as f:
            self.assertEqual(f.read().splitlines(), ["sub-old list", "sub-old show"])
        with open(self.log + ".account") as f:
            self.assertEqual(f.read().strip(), "sub-new")
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())
        self.assertNotIn("SETTINGS_KEYVAULT", self.local_text())

    def test_a_move_from_a_tenant_az_isnt_signed_in_to_says_how(self):
        self.local(**self.VAULT, AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("vault_detach_from sub-old sub-new", STUB_ACCOUNTS="sub-new")
        self.assertEqual(r.returncode, 1)
        self.assertIn("az login --tenant tenant-old", r.stderr)
        self.assertIn("--subscription sub-new", r.stderr)
        self.assertFalse(os.path.exists(self.log + ".vault"))
        self.assertIn("SETTINGS_KEYVAULT", self.local_text())

    def test_a_failed_move_selects_the_new_subscription_again(self):
        self.local(**self.VAULT)
        r = self.lib("vault_detach_from sub-old sub-new", STUB_VAULT_ERROR="ERROR: (Forbidden) Caller is not authorized.")
        self.assertEqual(r.returncode, 1)
        with open(self.log + ".account") as f:
            self.assertEqual(f.read().strip(), "sub-new")
        self.assertIn("SETTINGS_KEYVAULT", self.local_text())

    def test_bootstrap_detaches_before_selecting_the_new_subscription(self):
        with open(os.path.join(HERE, "bootstrap.sh")) as f:
            text = f.read()
        detach = text.index('vault_detach_from "$previous" "$SUBSCRIPTION"')
        select = text.index('az account set --subscription "$SUBSCRIPTION"')
        self.assertLess(detach, select)
        self.assertEqual(text.count("az account set"), 1)

    def test_migrate_moves_the_public_settings(self):
        self.local(USNM_JA_LATIN="true", USNM_INGEST_SCRATCH_GIB="64", USNM_API_IMAGE="",
                   USNM_ALERT_EMAILS="ops@example.org", API_URL="x")
        self.committed({"USNM_INGEST_SCRATCH_GIB": 64})
        r = self.lib("migrate_settings; settings_check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data(), {"$schema": "./schema.json", "USNM_INGEST_SCRATCH_GIB": 64,
                                                 "USNM_JA_LATIN": True})
        self.assertEqual(self.local_text(), 'USNM_ALERT_EMAILS="ops@example.org"\nAPI_URL="x"\n')
        self.assertIn("private, stays", r.stdout)

    def test_migrate_stops_on_a_conflict(self):
        self.local(USNM_JA_LATIN="true", USNM_OCR="true")
        self.committed({"USNM_JA_LATIN": False})
        r = self.lib("migrate_settings")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_JA_LATIN", r.stderr)
        # Nothing moved.
        self.assertEqual(self.committed_data(), {"$schema": "./schema.json", "USNM_JA_LATIN": False})
        self.assertIn('USNM_OCR="true"', self.local_text())


class SettingsScript(Stubbed):
    def run_settings(self, *args, **extra):
        return subprocess.run([os.path.join(HERE, "settings.sh"), *args], cwd=ROOT,
                              env={**self.env, **extra}, capture_output=True, text=True)

    def test_a_public_setting_goes_in_the_committed_file(self):
        r = self.run_settings("--env", "dev", "--key", "USNM_JA_LATIN", "--value", "true")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data()["USNM_JA_LATIN"], True)
        self.assertIn("pull request", r.stdout)
        r = self.run_settings("--env", "dev", "--key", "USNM_JA_LATIN")
        self.assertEqual(r.stdout, "true\n")
        r = self.run_settings("--env", "dev", "--key", "USNM_JA_LATIN", "--unset")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("USNM_JA_LATIN", self.committed_data())

    def test_a_private_setting_with_a_vault_goes_there(self):
        # A local copy (from before the vault) goes, so it can't hide the
        # vault's value and stop the provision.
        self.settings_file('SETTINGS_KEYVAULT="usnmcorekvdevabc123"\nUSNM_ALERT_EMAILS="old@example.org"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("scripts/provision.sh --env dev", r.stdout)
        self.assertEqual(self.local_text(), 'SETTINGS_KEYVAULT="usnmcorekvdevabc123"\n')
        self.assertIn("keyvault secret set --vault-name usnmcorekvdevabc123 --name USNM-ALERT-EMAILS "
                      "--value ops@example.org -o none", self.calls())
        r = self.lib("load_vault; settings_check", STUB_SECRETS="USNM-ALERT-EMAILS",
                     STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("USNM_ALERT_EMAILS", r.stderr)

    def test_a_refused_vault_write_keeps_the_local_copy(self):
        self.settings_file('SETTINGS_KEYVAULT="usnmcorekvdevabc123"\nUSNM_ALERT_EMAILS="old@example.org"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org",
                              STUB_VAULT_ERROR="ERROR: (Forbidden) Caller is not authorized.")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't set USNM_ALERT_EMAILS", r.stderr)
        self.assertIn('USNM_ALERT_EMAILS="old@example.org"', self.local_text())

    def test_a_private_setting_is_read_from_the_vault(self):
        self.settings_file('SETTINGS_KEYVAULT="usnmcorekvdevabc123"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS",
                          STUB_SECRETS="USNM-ALERT-EMAILS", STUB_SECRET_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "ops@example.org\n")

    def test_a_private_setting_without_a_vault_stays_local(self):
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())

    def test_migrate(self):
        self.settings_file('USNM_OCR="true"\n')
        r = self.run_settings("--env", "dev", "--migrate")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data()["USNM_OCR"], True)
        self.assertIn("moved 1", r.stdout)

    def test_migrate_takes_only_the_environment(self):
        r = self.run_settings("--env", "dev", "--migrate", "--key", "USNM_OCR")
        self.assertEqual(r.returncode, 2)


class ProvisionScript(Stubbed):
    def provision(self, *args):
        return subprocess.run([os.path.join(HERE, "provision.sh"), "--env", "dev", *args], cwd=ROOT,
                              env=self.env, capture_output=True, text=True)

    def test_uncommitted_settings_are_refused(self):
        self.committed({"USNM_OCR": True}, commit=False)
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("uncommitted changes", r.stderr)
        self.assertFalse(os.path.exists(self.log))

    def test_allow_dirty_deploys_them(self):
        self.committed({"USNM_OCR": True}, commit=False)
        r = self.provision("--allow-dirty")
        # Past the check: the stub az then fails the version check.
        self.assertNotIn("uncommitted", r.stderr)
        self.assertTrue(os.path.exists(self.log))

    def test_committed_settings_pass(self):
        self.committed({"USNM_OCR": True})
        r = self.provision()
        self.assertNotIn("uncommitted", r.stderr)


class CheckCommitted(unittest.TestCase):
    """scripts/lib/env_settings.py --check, on a directory of its own."""

    def setUp(self):
        self.dir = tempfile.mkdtemp()
        shutil.copy(os.path.join(ROOT, "infra", "env", "schema.json"), self.dir)

    def tearDown(self):
        shutil.rmtree(self.dir)

    def check(self, text, name="prod.json"):
        with open(os.path.join(self.dir, name), "w") as f:
            f.write(text)
        return subprocess.run(["python3", os.path.join(HERE, "lib", "env_settings.py"), "--check"],
                              env={**os.environ, "USNM_ENV_DIR": self.dir}, capture_output=True, text=True)

    def test_the_checkouts_files(self):
        r = subprocess.run(["python3", os.path.join(HERE, "lib", "env_settings.py"), "--check"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_valid_file(self):
        r = self.check('{\n  "$schema": "./schema.json",\n  "USNM_JA_LATIN": true,\n  "USNM_SEARCH_BACKEND": "quickwit"\n}\n')
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_private_setting_fails(self):
        r = self.check('{\n  "USNM_OCR_NODE_IPS": "203.0.113.7"\n}\n')
        self.assertEqual(r.returncode, 1)
        self.assertIn("private", r.stderr)

    def test_a_local_setting_fails(self):
        r = self.check('{\n  "AZURE_LOCATION": "eastus2"\n}\n')
        self.assertEqual(r.returncode, 1)
        self.assertIn("local", r.stderr)

    def test_an_unknown_setting_fails(self):
        r = self.check('{\n  "USNM_NO_SUCH": true\n}\n')
        self.assertEqual(r.returncode, 1)
        self.assertIn("isn't a setting", r.stderr)

    def test_the_wrong_type_fails(self):
        for text in ('{\n  "USNM_JA_LATIN": "true"\n}\n', '{\n  "USNM_SPLIT_DOCS": true\n}\n'):
            r = self.check(text)
            self.assertEqual(r.returncode, 1, text)

    def test_unsorted_keys_fail(self):
        r = self.check('{\n  "USNM_OCR": true,\n  "USNM_JA_LATIN": true\n}\n')
        self.assertEqual(r.returncode, 1)
        self.assertIn("sorted", r.stderr)

    def test_another_format_fails(self):
        r = self.check('{"USNM_OCR": true}\n')
        self.assertEqual(r.returncode, 1)
        self.assertIn("canonical", r.stderr)

    def test_a_setting_missing_from_the_schema_fails(self):
        with open(os.path.join(self.dir, "schema.json")) as f:
            schema = json.load(f)
        del schema["properties"]["USNM_JA_LATIN"]
        with open(os.path.join(self.dir, "schema.json"), "w") as f:
            json.dump(schema, f)
        r = self.check("{}\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("reads USNM_JA_LATIN", r.stderr)


if __name__ == "__main__":
    unittest.main()
