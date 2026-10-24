"""Tests for the dev lifecycle pieces of scripts/lib/env.sh and for
scripts/azure/dev-status.sh (#275), against a stub `az` (python3 -m unittest
scripts/azure/test_env_scripts.py). Nothing here reaches Azure.

The stub answers from environment variables: STUB_STACK (the stack's
resource count; unset: not found), STUB_GROUPS (group names, one per line),
STUB_TAGS (the project group's experiment and started, one value per line;
empty lines for missing tags; unset: no group), STUB_TAGS_ERROR (reading
the tags fails with that message),
STUB_TAGGED_GROUP (when set, the only group `group show` finds),
STUB_PARAMS (the guard-rail stack's parameters as two tsv rows) and
STUB_COST (Cost Management's answer; STUB_COST_ERROR makes it fail with that
message, for STUB_COST_FAILS calls when set; then STUB_COST_NEXT_ERROR
if set, otherwise success). Cost calls are counted in STUB_LOG.cost. For a job execution (scripts/azure/start-job.sh, and
scripts/azure/titles-seed.sh through it): STUB_EXECUTION (the started execution's name) and
STUB_EXEC_STATUS (its status); STUB_JOB_TEMPLATE (the job's template, JSON; unset: one
container, cr/usnewsmap-ingest:exp-1). For the settings store (#295): STUB_KEYS
(the keys under the label, one per line), STUB_KV_<KEY> (a key's value),
STUB_STORE_ERROR (every App Configuration call fails with that message),
STUB_STORE_FAILS (the first that many are refused); STUB_LIVE_VAULT (the
one Key Vault `keyvault show` finds);
it refuses a call without --auth-mode login. STUB_OUTPUTS (a stack's
outputs as JSON), STUB_VALIDATE_ERROR (`stack sub validate` fails with that
message), STUB_DELETED_VAULTS (soft-deleted vaults, one per line: the name,
a tab and the vault id) and
STUB_GROUP_EXISTS (`az group exists`, for any group; unset: false) or
STUB_EXISTING_GROUPS (when set, the only groups `az group exists` finds,
one per line). For the public-network policy's assignment (#336):
STUB_ASSIGNMENT_GROUP (the one group that holds it; unset: none),
STUB_ASSIGNED_IDS (its publicAccountIds, JSON), STUB_ASSIGNMENT_ERROR
(reading it fails with that message) and STUB_EXEMPTIONS_WHATIF (the
policy step's what-if, JSON).
`az account set` selects a subscription (STUB_ACCOUNTS: the ones az is
signed in to, one per line; unset: any), and each store call is logged
with the selected one, the operation and the label to STUB_LOG.store, and
each read of the stack's outputs
the selected one to STUB_LOG.stack. STUB_OUTPUTS_ERROR makes reading the
stack's outputs fail with that message. It logs each call to STUB_LOG.

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
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(HERE)
ROOT = os.path.dirname(SCRIPTS)

STUB_AZ = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2" in
  "account show") case "$*" in *"--query id"*) echo sub-1 ;; *"--query tenantId"*) echo tenant-1 ;; esac ;;
  "account set") s=$(sed 's/.*--subscription \([^ ]*\).*/\1/' <<< "$*")
    if [ -n "${STUB_ACCOUNTS:-}" ] && ! printf '%b\n' "$STUB_ACCOUNTS" | grep -qx "$s"; then
      echo "ERROR: The subscription of '$s' doesn't exist in cloud 'AzureCloud'." >&2; exit 1
    fi
    echo "$s" > "$STUB_LOG.account" ;;
  "stack sub")
    case "$*" in
      "stack sub list"*)
        if [ -n "${STUB_STACK_LIST_ERROR:-}" ]; then echo "$STUB_STACK_LIST_ERROR" >&2; exit 1; fi
        printf '%s\n' "${STUB_STACK_NAMES:-}" ;;
      "stack sub create"*) ;;
      "stack sub delete"*) ;;
      "stack sub validate"*)
        if [ -n "${STUB_VALIDATE_ERROR:-}" ]; then echo "$STUB_VALIDATE_ERROR" >&2; exit 1; fi ;;
      *"--query resources -o json"*)
        if [ -n "${STUB_RESOURCES_ERROR:-}" ]; then echo "$STUB_RESOURCES_ERROR" >&2; exit 1; fi
        echo "${STUB_RESOURCES:-[]}" ;;
      *"--query resources[].id"*) printf '%s\n' "${STUB_RESOURCE_IDS:-}" ;;
      *"--query location"*) echo eastus2 ;;
      *"keys(parameters"*) printf '%s\n' "${STUB_PARAMS:-}" ;;
      *"--query outputs -o json"*)
        echo "$(cat "$STUB_LOG.account" 2> /dev/null || echo none)" >> "$STUB_LOG.stack"
        if [ -n "${STUB_OUTPUTS_ERROR:-}" ]; then echo "$STUB_OUTPUTS_ERROR" >&2; exit 1
        elif [ -n "${STUB_OUTPUTS:-}" ]; then printf '%s\n' "$STUB_OUTPUTS"
        elif [ -n "${STUB_STACK:-}" ]; then echo '{}'
        else echo "ERROR: DeploymentStackNotFound: usnm-dev could not be found." >&2; exit 3; fi ;;
      *) if [ -n "${STUB_STACK:-}" ]; then echo "$STUB_STACK"
         else echo "ERROR: DeploymentStackNotFound: usnm-dev could not be found." >&2; exit 3; fi ;;
    esac ;;
  "appconfig kv")
    printf '%s %s %s\n' "$(cat "$STUB_LOG.account" 2> /dev/null || echo none)" "$3" \
      "$(sed -n 's/.*--label \([^ ]*\).*/\1/p' <<< "$*")" >> "$STUB_LOG.store"
    case "$*" in *"--auth-mode login"*) ;; *) echo "stub az: the store takes Entra ID only: $*" >&2; exit 9 ;; esac
    if [ -n "${STUB_STORE_ERROR:-}" ]; then echo "$STUB_STORE_ERROR" >&2; exit 1; fi
    if [ "$(wc -l < "$STUB_LOG.store")" -le "${STUB_STORE_FAILS:-0}" ]; then
      echo "ERROR: Operation returned an invalid status 'Forbidden'" >&2; exit 1; fi
    k=$(sed -n 's/.*--key \([^ ]*\).*/\1/p' <<< "$*")
    case "$3" in
      list) if [ -n "$k" ]; then v="STUB_KV_$k"; printf '%s\n' "${!v:-}"
        else python3 -c 'import json, os
keys = [k for k in os.environ.get("STUB_KEYS", "").replace("\\n", "\n").split("\n") if k]
print(json.dumps([{"key": k, "value": os.environ.get("STUB_KV_" + k, "")} for k in keys]))'; fi ;;
      set|delete) ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "stack-whatif sub")
    if [ -n "${STUB_STACK_WHATIF_ERROR:-}" ]; then echo "$STUB_STACK_WHATIF_ERROR" >&2; exit 1; fi
    echo "stack what-if: no changes" ;;
  "deployment sub")
    if [ -n "${STUB_CAPTURE_PARAMS:-}" ]; then
      printf '%s\t%s\t%s\t%s\n' "${USNM_ARCHIVE_ACCOUNT:-}" "${USNM_SEARCH_CLUSTER:-}" \
        "${USNM_SEARCH_LOCAL_MODE:-}" "${USNM_INGEST_SCRATCH_GIB:-}" >> "$STUB_LOG.params"
    fi
    if [ -n "${STUB_STACK_WHATIF_ERROR:-}" ]; then echo "$STUB_STACK_WHATIF_ERROR" >&2; exit 1; fi
    case "$*" in
      *--no-pretty-print*) if [ -n "${STUB_STACK_WHATIF:-}" ]; then printf '%s\n' "$STUB_STACK_WHATIF"; else echo '{"changes": []}'; fi ;;
      *) echo "what-if: no changes" ;;
    esac ;;
  "policy assignment")
    if [ -n "${STUB_ASSIGNMENT_ERROR:-}" ]; then echo "$STUB_ASSIGNMENT_ERROR" >&2; exit 1; fi
    if [ -n "${STUB_ASSIGNMENT_GROUP:-}" ] && [[ "$*" == *"/resourceGroups/$STUB_ASSIGNMENT_GROUP "* ]]; then
      case "$*" in *publicAccountIds*) echo "${STUB_ASSIGNED_IDS:-[]}" ;; *) echo usnm-audit-data-public-network ;; esac
    else
      echo "ERROR: (PolicyAssignmentNotFound) The policy assignment 'usnm-audit-data-public-network' is not found." >&2; exit 3
    fi ;;
  "deployment group")
    case "$*" in
      *what-if*--no-pretty-print*)
        if [ -n "${STUB_EXEMPTIONS_WHATIF:-}" ]; then printf '%s\n' "$STUB_EXEMPTIONS_WHATIF"; else echo '{"changes": []}'; fi ;;
      *what-if*) echo "what-if: the policy step" ;;
      "deployment group create"*) ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "group list") [ -z "${STUB_GROUPS:-}" ] || printf '%s\n' "$STUB_GROUPS" ;;
  "ad group")
    case "$*" in
      "ad group list"*) printf '%s\n' "${STUB_NODE_GROUP_IDS:-}" ;;
      "ad group create"*) echo new-group-id ;;
      *"--query displayName"*) echo "${STUB_NODE_GROUP_NAME:-usnm-ocr-nodes-dev}" ;;
      *"--query mailNickname"*) echo "${STUB_NODE_GROUP_NICKNAME:-usnm-ocr-nodes-dev}" ;;
      *"member list"*) echo 2 ;;
      *"owner list"*) ;;
      *) echo "stub az: unexpected group mutation: $*" >&2; exit 9 ;;
    esac ;;
  "ad signed-in-user") ;;
  "group exists")
    if [ -n "${STUB_EXISTING_GROUPS+x}" ]; then
      if printf '%b\n' "$STUB_EXISTING_GROUPS" | grep -qx "$4"; then echo true; else echo false; fi
    else echo "${STUB_GROUP_EXISTS:-false}"; fi ;;
  "group create") ;;
  "group delete") ;;
  "identity list"|"role assignment"|"storage account")
    case "$*" in
      "identity list"*|"role assignment list"*|"storage account show"*) ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "keyvault list-deleted") [ -z "${STUB_DELETED_VAULTS:-}" ] || printf '%b\n' "$STUB_DELETED_VAULTS" ;;
  "keyvault recover") ;;
  "keyvault show")
    if [ -n "${STUB_LIVE_VAULT:-}" ] && [ "$4" = "$STUB_LIVE_VAULT" ]; then echo "$4"
    else echo "ERROR: (ResourceNotFound) The Resource 'Microsoft.KeyVault/vaults/$4' was not found." >&2; exit 3; fi ;;
  "group show")
    if [ -n "${STUB_TAGS_ERROR:-}" ]; then echo "$STUB_TAGS_ERROR" >&2; exit 1; fi
    if [ -n "${STUB_TAGGED_GROUP:-}" ] && [ "$4" != "$STUB_TAGGED_GROUP" ]; then
      echo "ERROR: (ResourceGroupNotFound) Resource group '$4' could not be found." >&2; exit 3; fi
    case "$*" in *"--query [tags.experiment || '', tags.started || ''] -o tsv"*) ;;
      *) echo "stub az: unexpected tags query: $*" >&2; exit 9 ;; esac
    if [ -n "${STUB_TAGS+x}" ]; then printf '%b\n' "$STUB_TAGS"
    else echo "ERROR: (ResourceGroupNotFound) Resource group 'rg-usnm-dev' could not be found." >&2; exit 3; fi ;;
  "resource list") printf 'Microsoft.Storage/storageAccounts\tstusnmdev\n' ;;
  "containerapp job")
    case "$*" in
      *"execution show"*) echo "${STUB_EXEC_STATUS:-Succeeded}" ;;
      *"job show"*) if [ -n "${STUB_JOB_TEMPLATE:-}" ]; then printf '%s\n' "$STUB_JOB_TEMPLATE"
        else echo '{"containers":[{"name":"ingest","image":"cr/usnewsmap-ingest:exp-1","args":["run"],"env":[]}]}'; fi ;;
      *) echo "stub az: unexpected: $*" >&2; exit 9 ;;
    esac ;;
  "rest --method")
    case "$*" in *"/jobs/"*"/start?"*) echo "${STUB_EXECUTION:-}"; exit 0 ;; esac
    count=$(cat "$STUB_LOG.cost" 2> /dev/null || echo 0)
    count=$((count + 1)); echo "$count" > "$STUB_LOG.cost"
    if [ "$count" -le "${STUB_COST_FAILS:-999}" ]; then
      if [ -n "${STUB_COST_ERROR:-}" ]; then echo "$STUB_COST_ERROR" >&2; exit 1; fi
    elif [ -n "${STUB_COST_NEXT_ERROR:-}" ]; then echo "$STUB_COST_NEXT_ERROR" >&2; exit 1; fi
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
        self.env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "HOME": self.tmp, "TMPDIR": self.tmp,
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


class SearcherRollbackSettings(Stubbed):
    def test_public_slot_budget_and_max_replicas_are_exported(self):
        self.committed({"USNM_COMPUTE_CONCURRENCY": 8, "USNM_API_MAX_REPLICAS": 3})
        run = self.lib('export_stack_settings; printf "%s/%s" '
                       '"$USNM_COMPUTE_CONCURRENCY" "$USNM_API_MAX_REPLICAS"')
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(run.stdout, "8/3")

    def test_public_retention_is_exported_for_both_provision_paths(self):
        for retain in (True, False):
            with self.subTest(retain=retain):
                self.committed({"USNM_SEARCH_BACKEND": "quickwit",
                                "USNM_SEARCHER_APP_RETAIN": retain})
                r = self.lib('export_stack_settings; printf "%s/%s" '
                             '"${USNM_SEARCHER_APP:-false}" "$USNM_SEARCHER_APP_RETAIN"')
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout, f"false/{str(retain).lower()}")

    def test_unset_retention_ignores_callers_environment(self):
        r = self.lib('export_stack_settings; echo "${USNM_SEARCHER_APP_RETAIN:-false}"',
                     USNM_SEARCHER_APP_RETAIN="true")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "false")


@unittest.skipUnless(shutil.which(os.environ.get("BICEP", "bicep")), "Bicep CLI required")
class DnsWhatIfTemplate(unittest.TestCase):
    def test_cname_loop_count_does_not_depend_on_runtime_fqdn(self):
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build",
                              "infra/modules/dns.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        template = json.loads(run.stdout)
        resources = template["resources"]
        if isinstance(resources, dict):
            resources = list(resources.values())
        hosts = next(r for r in resources if r["type"] == "Microsoft.Network/dnsZones/CNAME")
        self.assertNotIn("appFqdn", hosts["copy"]["count"])
        self.assertIn("'www'", hosts["copy"]["count"])
        self.assertIn("'api'", hosts["copy"]["count"])
        self.assertEqual(hosts["condition"], "[parameters('deployAppRecords')]")
        self.assertEqual(template["parameters"]["deployAppRecords"]["defaultValue"],
                         "[not(empty(parameters('appFqdn')))]")
        self.assertEqual(hosts["properties"]["CNAMERecord"]["cname"], "[parameters('appFqdn')]")
        zone = next(r for r in resources if r["type"] == "Microsoft.Network/dnsZones")
        self.assertNotIn("condition", zone)
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build", "infra/main.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        root = json.loads(run.stdout)
        dns = root["resources"]["dns"]
        self.assertEqual(dns["properties"]["parameters"]["deployAppRecords"]["value"],
                         "[variables('deployApi')]")


@unittest.skipUnless(shutil.which(os.environ.get("BICEP", "bicep")), "Bicep CLI required")
class IngestTimeoutTemplate(unittest.TestCase):
    def test_only_explicit_full_or_staging_extends_the_ingest_timeout(self):
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build",
                              "infra/modules/ingestjobs.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        template = json.loads(run.stdout)
        self.assertEqual(template["variables"]["ingestReplicaTimeoutSecs"],
                         "[if(or(parameters('full'), parameters('stageIndexStorage')), 345600, 172800)]")
        resources = template["resources"]
        if isinstance(resources, dict):
            resources = list(resources.values())
        ingest = next(r for r in resources if r.get("name") == "[parameters('ingestJobName')]")
        backfill = next(r for r in resources if r.get("name") == "[parameters('backfillJobName')]")
        config = ingest["properties"]["configuration"]
        self.assertEqual(config["replicaTimeout"], "[variables('ingestReplicaTimeoutSecs')]")
        self.assertEqual(config["replicaRetryLimit"], 0)
        self.assertEqual(template["variables"]["replicaTimeoutSecs"], 86400)
        self.assertEqual(backfill["properties"]["configuration"]["replicaTimeout"],
                         "[variables('replicaTimeoutSecs')]")

    def test_only_scratch_jobs_place_the_bounded_wal_on_local_disk(self):
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build",
                              "infra/modules/ingestjobs.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        template = json.loads(run.stdout)
        scratch = template["variables"]["scratchEnv"]
        self.assertTrue(scratch.startswith("[if(variables('scratch'),"))
        self.assertIn("'USNM_WRITER_WAL_DIR'", scratch)
        self.assertIn("'/work'", scratch)
        resources = template["resources"]
        if isinstance(resources, dict):
            resources = list(resources.values())
        ingest = next(r for r in resources if r.get("name") == "[parameters('ingestJobName')]")
        self.assertIn("variables('scratchEnv')", ingest["properties"]["template"]["containers"][0]["env"])



@unittest.skipUnless(shutil.which(os.environ.get("BICEP", "bicep")), "Bicep CLI required")
class SearcherRollbackTemplate(unittest.TestCase):
    """Offline ARM contracts: retention keeps resources, never API routing."""

    @classmethod
    def setUpClass(cls):
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build",
                              "infra/modules/containerapp.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        cls.template = json.loads(run.stdout)

    def test_api_consumes_allocated_slots_without_nested_user_functions(self):
        self.assertNotIn("functions", self.template)
        self.assertIsInstance(self.template["resources"], list)
        variables = self.template["variables"]
        self.assertEqual(variables["computeEnv"],
                         [{"name": "USNM_COMPUTE_CONCURRENCY", "value": "[string(parameters('computeSlots'))]"}])
        self.assertIn("variables('computeEnv')", variables["apiContainer"]["env"])
        app = next(r for r in self.template["resources"] if r["name"] == "[parameters('name')]")
        self.assertEqual(app["properties"]["template"]["scale"]["maxReplicas"], "[parameters('maxReplicas')]")
        self.assertEqual(self.template["parameters"]["maxReplicas"]["minValue"], 1)
        self.assertEqual(self.template["parameters"]["computeSlots"]["minValue"], 1)

    def test_main_passes_the_same_replica_cap_and_searcher_budget(self):
        run = subprocess.run([os.environ.get("BICEP", "bicep"), "build", "infra/main.bicep", "--stdout"],
                             cwd=ROOT, capture_output=True, text=True, check=True)
        template = json.loads(run.stdout)
        resources = template["resources"]
        if isinstance(resources, dict):
            resources = list(resources.values())
        api = next(r for r in resources if r.get("name") == "api")
        self.assertEqual(api["properties"]["parameters"]["maxReplicas"]["value"], "[parameters('apiMaxReplicas')]")
        self.assertEqual(api["properties"]["parameters"]["computeSlots"]["value"], "[variables('apiComputeSlots')]")
        self.assertEqual(template["variables"]["apiComputeSlots"],
                         "[__bicep.replicaComputeSlots(parameters('computeConcurrency'), parameters('apiMaxReplicas'), and(equals(parameters('searchBackend'), 'quickwit'), or(parameters('searcherApp'), parameters('indexStorageServe'))))]")

    def test_slot_budget_rounds_down_and_sidecars_keep_the_budget(self):
        from pathlib import Path
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            base = Path(directory)
            (base / "template.bicep").write_text("param slots int\noutput actual int = slots\n")
            for budget, replicas, shared, expected in [
                (2, 2, True, 1), (2, 1, True, 2), (8, 2, True, 4),
                (8, 3, True, 2), (3, 2, True, 1), (2, 2, False, 2),
                (1, 8, False, 1), (1, 2, True, None), (0, 2, True, None),
                (2, 0, True, None),
            ]:
                with self.subTest(budget=budget, replicas=replicas, shared=shared):
                    params = base / "case.bicepparam"
                    params.write_text(
                        "using './template.bicep'\n"
                        "import { replicaComputeSlots } from '../infra/api-capacity.bicep'\n"
                        f"param slots = replicaComputeSlots({budget}, {replicas}, {str(shared).lower()})\n")
                    run = subprocess.run([os.environ.get("BICEP", "bicep"), "build-params", str(params), "--stdout"],
                                         capture_output=True, text=True)
                    if expected is None:
                        self.assertNotEqual(run.returncode, 0)
                    else:
                        self.assertEqual(run.returncode, 0, run.stderr)
                        result = json.loads(run.stdout)
                        self.assertEqual(json.loads(result["parametersJson"])["parameters"]["slots"]["value"], expected)

    def test_app_and_reader_share_retention_condition(self):
        variables = self.template["variables"]
        self.assertEqual(variables["searcherDeployed"],
                         "[and(variables('quickwit'), or(parameters('searcherApp'), parameters('searcherAppRetain')))]")
        searcher = next(r for r in self.template["resources"]
                        if r["name"] == "[parameters('searcherName')]")
        reader = next(r for r in self.template["resources"]
                      if r["type"] == "Microsoft.Authorization/roleAssignments"
                      and "parameters('searcherName')" in r["name"])
        for resource in (searcher, reader):
            self.assertEqual(resource["condition"], "[variables('searcherDeployed')]")
        self.assertFalse(self.template["parameters"]["searcherAppRetain"]["defaultValue"])

    def test_retention_does_not_select_api_backend_or_monitoring(self):
        variables = self.template["variables"]
        self.assertEqual(variables["searcherOn"],
                         "[and(variables('quickwit'), parameters('searcherApp'))]")
        self.assertEqual(variables["sidecar"],
                         "[and(and(variables('quickwit'), not(parameters('searcherApp'))), not(parameters('indexStorageServe')))]")
        self.assertIn("if(variables('searcherOn'), format('http://{0}', parameters('searcherName')), 'http://127.0.0.1:7280')",
                      variables["backendEnv"])
        self.assertEqual(self.template["outputs"]["searcherName"]["value"],
                         "[if(parameters('indexStorageServe'), parameters('indexSearcherName'), if(variables('searcherOn'), parameters('searcherName'), ''))]")
        app = next(r for r in self.template["resources"] if r["name"] == "[parameters('name')]")
        self.assertEqual(app["properties"]["template"]["containers"],
                         "[if(variables('sidecar'), createArray(variables('apiContainer'), variables('quickwitContainer')), createArray(variables('apiContainer')))]")

    def test_api_identity_and_both_readers_drop_with_searcher_app(self):
        self.assertEqual(self.template["variables"]["apiSystemIdentity"],
                         "[and(variables('quickwit'), not(parameters('searcherApp')))]")
        app = next(r for r in self.template["resources"] if r["name"] == "[parameters('name')]")
        self.assertEqual(app["identity"]["type"],
                         "[if(variables('apiSystemIdentity'), 'SystemAssigned,UserAssigned', 'UserAssigned')]")
        readers = [r for r in self.template["resources"]
                   if r["type"] == "Microsoft.Authorization/roleAssignments"
                   and "parameters('name')" in r["name"]]
        self.assertEqual(len(readers), 2)
        for reader in readers:
            expected = ("[and(not(empty(parameters('indexStorageAccountName'))), variables('apiSystemIdentity'))]"
                        if "parameters('indexStorageAccountName')" in reader["name"] else "[variables('apiSystemIdentity')]")
            self.assertEqual(reader["condition"], expected)
            self.assertIn(".identity.principalId", reader["properties"]["principalId"])
            self.assertIn("[resourceId('Microsoft.App/containerApps', parameters('name'))]", reader["dependsOn"])

    def test_index_storage_cutover_keeps_old_sidecar_identity(self):
        variables = self.template["variables"]
        # indexStorageServe=true removes the new revision's sidecar, not
        # the app-level identity needed by its still-serving old revision.
        self.assertEqual(variables["sidecar"],
                         "[and(and(variables('quickwit'), not(parameters('searcherApp'))), not(parameters('indexStorageServe')))]")
        self.assertEqual(variables["apiSystemIdentity"],
                         "[and(variables('quickwit'), not(parameters('searcherApp')))]")

    def test_rollback_keeps_single_revision_readiness_gated_handoff(self):
        app = next(r for r in self.template["resources"] if r["name"] == "[parameters('name')]")
        self.assertEqual(app["properties"]["configuration"]["activeRevisionsMode"], "Single")
        probes = self.template["variables"]["apiContainer"]["probes"]
        for kind in ("Startup", "Readiness"):
            probe = next(p for p in probes if p["type"] == kind)
            self.assertEqual(probe["httpGet"]["path"], "/readyz")


class SettingsLocation(Stubbed):
    def test_the_shared_directory_wins(self):
        r = self.lib('echo "$ENV_FILE"')
        self.assertEqual(r.stdout.strip(), os.path.join(self.settings, "dev", ".env"))

    def test_without_it_the_checkout_holds_them(self):
        r = self.lib('echo "$ENV_FILE"', env_name="x1")
        self.assertEqual(r.stdout.strip(), ".azure/x1/.env")


class EnvironmentNames(Stubbed):
    # ADR-0017: prod, dev, or x plus 1-4 lowercase letters or digits.
    def test_the_closed_list(self):
        for name in ("prod", "dev", "x1", "xab12"):
            r = self.lib("true", env_name=name)
            self.assertEqual(r.returncode, 0, f"{name}: {r.stderr}")

    def test_anything_else_is_refused(self):
        # usnm-archive and usnm-guardrails are stacks of their own, ci is a
        # function word, and longer names overflow the resource names.
        for name in ("archive", "guardrails", "ci", "x", "xabcde", "Prod", "dev2", "staging", "x-1"):
            r = self.lib("true", env_name=name)
            self.assertEqual(r.returncode, 2, name)
            self.assertIn("ADR-0017", r.stderr)

    def test_bootstrap_checks_it_before_signing_in(self):
        # The stub az would log a call; the name is refused first.
        r = subprocess.run([os.path.join(HERE, "bootstrap.sh"), "--env", "staging"], cwd=ROOT,
                           env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("ADR-0017", r.stderr)
        self.assertFalse(os.path.exists(self.log))


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
                     STUB_TAGS="275\n2026-10-01T12:00:00Z")
        self.assertEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")

    def test_the_group_in_either_name_form(self):
        # Dev's project group is usnm-core-rg-dev (ADR-0017); rg-usnm-dev is
        # the legacy form, still looked for.
        self.settings_file('USNM_EXPERIMENT="275"\n')
        for group in ("usnm-core-rg-dev", "rg-usnm-dev"):
            r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED", STUB_TAGGED_GROUP=group,
                         STUB_TAGS="275\n2026-10-01T12:00:00Z")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), "2026-10-01T12:00:00Z", group)
        shows = [c for c in self.calls() if c.startswith("group show")]
        self.assertTrue(shows[0].startswith("group show -n usnm-core-rg-dev "), shows)
        self.assertTrue(shows[-1].startswith("group show -n rg-usnm-dev "), shows)

    def test_another_experiment_starts_again(self):
        self.settings_file('USNM_EXPERIMENT="301"\n')
        r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED",
                     STUB_TAGS="275\n2026-10-01T12:00:00Z")
        self.assertNotEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")
        self.assertRegex(r.stdout.strip(), r"^\d{4}-")

    def test_missing_tags_start_now(self):
        self.settings_file('USNM_EXPERIMENT="275"\n')
        for tags in ("275\n", "\n2026-10-01T12:00:00Z", "\n", ""):
            with self.subTest(tags=tags):
                r = self.lib("experiment_marker; aget USNM_EXPERIMENT_STARTED", STUB_TAGS=tags)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertRegex(r.stdout.strip(), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
                self.assertNotEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")

    def test_repeated_provisions_keep_the_initial_start(self):
        self.settings_file('USNM_EXPERIMENT="275"\n')
        r = self.lib("experiment_marker; experiment_marker; aget USNM_EXPERIMENT_STARTED",
                     STUB_TAGS="275\n2026-10-01T12:00:00Z")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")

    def test_dry_run_keeps_the_start_without_writing_settings(self):
        initial = 'USNM_EXPERIMENT="275"\n'
        self.settings_file(initial)
        r = self.lib('experiment_marker --dry-run; echo "$EXPERIMENT_STARTED_DRY_RUN"',
                     STUB_TAGS="275\n2026-10-01T12:00:00Z")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "2026-10-01T12:00:00Z")
        self.assertEqual(self.local_text(), initial)

    def test_tag_errors_do_not_reset_the_start(self):
        initial = 'USNM_EXPERIMENT="275"\nUSNM_EXPERIMENT_STARTED="2026-10-01T12:00:00Z"\n'
        self.settings_file(initial)
        for error in ("ERROR: (AuthorizationFailed) no access", "ERROR: (TooManyRequests) throttled"):
            with self.subTest(error=error):
                r = self.lib("experiment_marker", STUB_TAGS_ERROR=error)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("can't read the tags", r.stderr)
                self.assertEqual(self.local_text(), initial)


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
    def setUp(self):
        super().setUp()
        sleep = os.path.join(self.tmp, "bin", "sleep")
        with open(sleep, "w") as f:
            f.write('#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "$STUB_LOG.sleep"\n')
        os.chmod(sleep, 0o755)

    def retry_delays(self):
        path = self.log + ".sleep"
        if not os.path.exists(path):
            return []
        with open(path) as f:
            return [int(line) for line in f]

    def cost_calls(self):
        with open(self.log + ".cost") as f:
            return int(f.read())

    def status(self, **extra):
        return subprocess.run([os.path.join(HERE, "dev-status.sh"), "--subscription", "sub-1"], cwd=ROOT,
                              env={**self.env, "STUB_COST": COST, **extra}, capture_output=True, text=True)

    def test_nothing_there(self):
        r = self.status()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("dev holds nothing", r.stdout)

    def test_resources_without_a_tag(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev\nrg-usnm-dev-spot", STUB_TAGS="\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no experiment tag", r.stderr)

    def test_a_group_left_behind(self):
        r = self.status(STUB_GROUPS="ME_cae-usnm-dev_rg-usnm-dev_eastus2")
        self.assertEqual(r.returncode, 1)

    def test_retired_spot_group_is_still_detected_without_a_stack(self):
        r = self.status(STUB_GROUPS="rg-usnm-dev-spot")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("group rg-usnm-dev-spot: 1 resources", r.stdout)
        self.assertIn("no experiment tag", r.stderr)
        self.assertNotIn("dev holds nothing", r.stdout)

    def test_an_experiment(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev",
                        STUB_TAGS="275\n2026-10-01T12:00:00Z")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("experiment #275, started 2026-10-01T12:00:00Z", r.stdout)
        # dev's groups only, whatever their case: not prod's, not dev2's.
        self.assertIn("cost since 2026-10-01T12:00:00Z: 1.75 USD", r.stdout)
        self.assertIn('"from": "2026-10-01T12:00:00Z"', "\n".join(self.calls()))
        self.assertTrue(re.search(r"group rg-usnm-dev: 1 resources", r.stdout))

    def test_age_and_seven_day_warning(self):
        for days in (2, 7, 9):
            with self.subTest(days=days):
                started = (datetime.now(timezone.utc) - timedelta(days=days)).strftime("%Y-%m-%dT%H:%M:%SZ")
                r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev",
                                STUB_TAGS=f"275\n{started}")
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn(f"{days} days ago", r.stdout)
                self.assertIn(f"cost since {started}: 1.75 USD", r.stdout)
                self.assertEqual("more than 7 days" in r.stderr, days > 7)

    def test_missing_start_uses_month_cost_and_unknown_age(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("experiment #275, started at an unknown time", r.stdout)
        month = datetime.now(timezone.utc).strftime("%Y-%m-01T00:00:00Z")
        self.assertIn(f"cost since {month}: 1.75 USD", r.stdout)
        self.assertIn(f'"from": "{month}"', "\n".join(self.calls()))
        self.assertNotIn("days ago", r.stdout)
        self.assertNotIn("more than 7 days", r.stderr)

    def test_missing_experiment_does_not_shift_start_into_its_slot(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev",
                        STUB_TAGS="\n2026-10-01T12:00:00Z")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("no experiment tag", r.stderr)

    def test_tag_errors_fail_before_requesting_cost(self):
        for error in ("ERROR: (AuthorizationFailed) no access", "ERROR: (TooManyRequests) throttled"):
            with self.subTest(error=error):
                r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS_ERROR=error)
                self.assertEqual(r.returncode, 1)
                self.assertIn("can't read the tags", r.stderr)
                self.assertFalse(any(c.startswith("rest --method") for c in self.calls()))

    def test_an_experiment_in_adr_0017_names(self):
        cost = json.dumps({"properties": {
            "columns": [{"name": "Cost"}, {"name": "ResourceGroupName"}, {"name": "Currency"}],
            "rows": [[1.25, "usnm-core-rg-dev", "USD"], [0.5, "USNM-CORE-INFRA-RG-DEV", "USD"],
                     [0.25, "rg-usnm-dev-spot", "USD"], [40.0, "rg-usnm-prod", "USD"],
                     [0.1, "usnm-core-rg-dev2", "USD"]]}})
        r = self.status(STUB_STACK="120", STUB_GROUPS="usnm-core-rg-dev\nusnm-core-infra-rg-dev",
                        STUB_TAGGED_GROUP="usnm-core-rg-dev", STUB_TAGS="275\n2026-10-01T12:00:00Z",
                        STUB_COST=cost)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("experiment #275, started 2026-10-01T12:00:00Z", r.stdout)
        self.assertIn("cost since 2026-10-01T12:00:00Z: 2 USD", r.stdout)
        self.assertTrue(re.search(r"group usnm-core-infra-rg-dev: 1 resources", r.stdout))
        query = [c for c in self.calls() if c.startswith("group list")][0]
        for g in ("usnm-core-rg-dev", "usnm-core-infra-rg-dev", "rg-usnm-dev-spot", "rg-usnm-dev",
                  "ME_cae-usnm-dev_"):
            self.assertIn(f"'{g}'", query)

    def test_cost_without_the_reader_role_is_a_warning(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\n2026-10-01T12:00:00Z",
                        STUB_COST_ERROR="ERROR: (AuthorizationFailed) no access")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("needs Cost Management Reader", r.stderr)

    def test_throttle_then_success_honours_retry_headers(self):
        for error, delay in (
            ('ERROR: Too Many Requests({"error":{"code":"429"}})', 2),
            ('ERROR: request failed {"error":{"code":"429"}}', 2),
            ('ERROR: HTTP/1.1 429 throttled', 2),
            ('ERROR: (TooManyRequests) throttled\nRetry-After: 7', 7),
            ('ERROR: 429 {"Retry-After": "09"}', 9),
            ("ERROR: 429 {'x-ms-ratelimit-microsoft.costmanagement-qpu-retry-after': '5', "
             "'x-ms-ratelimit-microsoft.costmanagement-entity-retry-after': '11'}", 11),
            ('ERROR: 429\nRetry-After: invalid', 2),
            ('ERROR: 429\nRetry-After: 0', 2),
            ('ERROR: 429\nRetry-After: Wed, 21 Oct 2015 07:28:00 GMT', 2),
        ):
            with self.subTest(error=error):
                for suffix in (".cost", ".sleep"):
                    path = self.log + suffix
                    if os.path.exists(path):
                        os.remove(path)
                r = self.status(STUB_COST_ERROR=error, STUB_COST_FAILS="1")
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("1.75 USD", r.stdout)
                self.assertNotIn("cost unknown", r.stderr)
                self.assertEqual(self.retry_delays(), [delay])
                self.assertEqual(self.cost_calls(), 2)

    def test_http_date_retry_after(self):
        from email.utils import format_datetime
        after = format_datetime(datetime.now(timezone.utc) + timedelta(seconds=30), usegmt=True)
        r = self.status(STUB_COST_ERROR=f"ERROR: 429\nRetry-After: {after}", STUB_COST_FAILS="1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.cost_calls(), 2)
        self.assertTrue(25 <= self.retry_delays()[0] <= 30, self.retry_delays())

    def test_persistent_throttle_leaves_empty_environment_successful(self):
        r = self.status(STUB_COST_ERROR="ERROR: (TooManyRequests) throttled")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("cost unknown", r.stderr)
        self.assertIn("dev holds nothing", r.stdout)
        self.assertNotIn("cost since", r.stdout)
        self.assertEqual(self.retry_delays(), [2, 4, 8])
        self.assertEqual(self.cost_calls(), 4)

    def test_persistent_throttle_keeps_experiment_successful(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\n",
                        STUB_COST_ERROR="ERROR: 429 throttled")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("cost unknown", r.stderr)
        self.assertIn("experiment #275", r.stdout)

    def test_throttle_does_not_hide_missing_experiment_tag(self):
        r = self.status(STUB_GROUPS="rg-usnm-dev", STUB_TAGS="\n",
                        STUB_COST_ERROR="ERROR: 429 throttled")
        self.assertEqual(r.returncode, 1)
        self.assertIn("cost unknown", r.stderr)
        self.assertIn("no experiment tag", r.stderr)

    def test_retry_wait_budget(self):
        r = self.status(STUB_COST_ERROR="ERROR: 429\nRetry-After: 30")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("cost unknown", r.stderr)
        self.assertEqual(self.retry_delays(), [30, 30])
        self.assertEqual(self.cost_calls(), 3)

    def test_over_budget_header_is_not_retried_early(self):
        r = self.status(STUB_COST_ERROR="ERROR: 429\nRetry-After: 61")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("cost unknown", r.stderr)
        self.assertEqual(self.retry_delays(), [])
        self.assertEqual(self.cost_calls(), 1)

    def test_non_throttle_error_after_retry_still_fails(self):
        r = self.status(STUB_COST_ERROR="ERROR: 429", STUB_COST_FAILS="1",
                        STUB_COST_NEXT_ERROR="ERROR: InternalServerError")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the cost", r.stderr)
        self.assertEqual(self.retry_delays(), [2])
        self.assertEqual(self.cost_calls(), 2)

    def test_request_id_containing_429_is_not_throttling(self):
        r = self.status(STUB_COST_ERROR="ERROR: InternalServerError request ID "
                        "03a429bf-1234-4321-aaaa-123456789abc")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the cost", r.stderr)
        self.assertNotIn("cost unknown", r.stderr)
        self.assertEqual(self.cost_calls(), 1)
        self.assertEqual(self.retry_delays(), [])

    def test_any_other_cost_error_fails(self):
        r = self.status(STUB_STACK="120", STUB_GROUPS="rg-usnm-dev", STUB_TAGS="275\n2026-10-01T12:00:00Z",
                        STUB_COST_ERROR="ERROR: InternalServerError")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the cost", r.stderr)
        self.assertEqual(self.cost_calls(), 1)
        self.assertEqual(self.retry_delays(), [])


class StartQuality(Stubbed):
    # The jobs' names come from the stack's outputs, in either form (ADR-0017).
    SETTINGS = ('AZURE_SUBSCRIPTION_ID="sub-1"\nAZURE_RESOURCE_GROUP="usnm-core-rg-dev"\n'
                'JA_OCR_JOB="usnm-jaocr-caj-dev"\nJA_AUDIT_JOB="usnm-jaone-caj-dev"\n')

    def start(self, *args):
        return subprocess.run([os.path.join(ROOT, "ja-ocr", "tools", "start-quality.sh"), "--env", "dev", *args],
                              cwd=ROOT, env={**self.env, "STUB_EXECUTION": "usnm-jaocr-caj-dev-abc12"},
                              capture_output=True, text=True)

    def test_the_jobs_come_from_the_settings(self):
        self.settings_file(self.SETTINGS)
        r = self.start("--sample-pct", "10")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(any(c.startswith("containerapp job show -n usnm-jaocr-caj-dev -g usnm-core-rg-dev")
                            for c in self.calls()))
        self.assertTrue(any("/jobs/usnm-jaocr-caj-dev/start?" in c for c in self.calls()))
        r = self.start("--command", "mixed")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(any("/jobs/usnm-jaone-caj-dev/start?" in c for c in self.calls()))

    def test_without_the_outputs(self):
        self.settings_file('AZURE_SUBSCRIPTION_ID="sub-1"\nAZURE_RESOURCE_GROUP="rg-usnm-dev"\n')
        r = self.start()
        self.assertEqual(r.returncode, 1)
        self.assertIn("JA_OCR_JOB", r.stderr)
        self.assertFalse(os.path.exists(self.log))


class StartJobWait(Stubbed):
    """scripts/azure/start-job.sh --wait-mins: the teardown runbook's titles-archive
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


class StartJobImageTag(Stubbed):
    """scripts/azure/start-job.sh --image-tag: one execution from another tag of the
    job's image, such as import-staged-run --current from a newer ingest image."""
    SETTINGS = StartJobWait.SETTINGS

    def start(self, *args, **extra):
        self.settings_file(self.SETTINGS)
        return subprocess.run([os.path.join(HERE, "start-job.sh"), "--env", "dev", "--job", "INGEST_JOB",
                               *args, "--", "import-staged-run", "--version", "pages-v20261022-1", "--current"],
                              cwd=ROOT, env={**self.env, "STUB_EXECUTION": "caj-usnm-ingest-dev-xyz34", **extra},
                              capture_output=True, text=True)

    def body(self):
        """The latest execution's body."""
        return json.loads([c for c in self.calls() if c.startswith("rest --method post")][-1]
                          .split("--body ", 1)[1].split(" --query", 1)[0])

    def test_without_it_the_job_s_image_runs(self):
        r = self.start()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.body()["containers"][0]["image"], "cr/usnewsmap-ingest:exp-1")
        self.assertNotIn("not the job's image", r.stderr)

    def test_only_the_first_container_s_tag_changes(self):
        sha = "4b7405f115cca3712b1376e3801e026751a418c3"
        r = self.start("--image-tag", sha)
        self.assertEqual(r.returncode, 0, r.stderr)
        first = self.body()["containers"][0]
        self.assertEqual(first["image"], f"cr/usnewsmap-ingest:{sha}")
        self.assertEqual(first["args"], ["import-staged-run", "--version", "pages-v20261022-1", "--current"])
        self.assertIn(f"runs cr/usnewsmap-ingest:{sha}, not the job's image", r.stderr)
        # The job itself is only read.
        self.assertFalse(any(c.startswith("containerapp job update") for c in self.calls()))

    def test_a_digest_goes_a_registry_port_stays(self):
        template = {"containers": [
            {"name": "ingest", "image": "cr.example:5000/usnewsmap-ingest:old@sha256:abc", "args": ["run"]},
            {"name": "side", "image": "cr.example:5000/side:1"}],
            "initContainers": [{"name": "init", "image": "cr.example:5000/init:1"}]}
        r = self.start("--image-tag", "v2.1_rc-1", STUB_JOB_TEMPLATE=json.dumps(template))
        self.assertEqual(r.returncode, 0, r.stderr)
        body = self.body()
        self.assertEqual([c["image"] for c in body["containers"]],
                         ["cr.example:5000/usnewsmap-ingest:v2.1_rc-1", "cr.example:5000/side:1"])
        self.assertEqual(body["initContainers"][0]["image"], "cr.example:5000/init:1")
        template["containers"][0]["image"] = "cr.example:5000/usnewsmap-ingest"
        r = self.start("--image-tag", "abc", STUB_JOB_TEMPLATE=json.dumps(template))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.body()["containers"][0]["image"], "cr.example:5000/usnewsmap-ingest:abc")

    def test_the_tag_is_checked_before_azure(self):
        for value in ("", "a/b", "a:b", "a@b", "a b", "x" * 129, "$(id)"):
            r = self.start("--image-tag", value)
            self.assertEqual(r.returncode, 2, repr(value))
            self.assertIn("--image-tag is an image tag", r.stderr)
            self.assertFalse(os.path.exists(self.log))
        self.assertEqual(self.start("--image-tag", "x" * 128).returncode, 0)


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
        # A private setting: the only call looks for the settings store in
        # its stack's outputs; there's no stack.
        # In the environment's recorded subscription.
        self.assertEqual(self.calls(), ["stack sub show -n usnm-config --query outputs -o json --subscription sub-1"])

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


class StackRenameGuard(Stubbed):
    RID = "/subscriptions/sub-1/resourceGroups/rg-usnm-dev/providers/Microsoft.Storage/storageAccounts/old"

    def setUp(self):
        super().setUp()
        vnet = ".".join(str(n) for n in (10, 99, 0, 0)) + "/24"
        self.settings_file(f'AZURE_SUBSCRIPTION_ID="sub-1"\nUSNM_VNET_ADDRESS_PREFIX="{vnet}"\n')

    def response(self, kind):
        return json.dumps({"changes": [{"resourceId": self.RID, "changeType": kind}]})

    def run_guard(self, script="deploy_stack", **extra):
        return self.lib(script, STUB_RESOURCES=json.dumps([{"id": self.RID}]), **extra)

    def test_guard_runs_before_create_with_same_parameters(self):
        r = self.run_guard(STUB_STACK_WHATIF=self.response("NoChange"))
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        plan = next(c for c in calls if c.startswith("deployment sub what-if"))
        create = next(c for c in calls if c.startswith("stack sub create"))
        self.assertIn("--parameters infra/main.bicepparam", plan)
        self.assertIn("--location eastus2", plan)
        self.assertIn("--parameters infra/main.bicepparam", create)
        self.assertLess(calls.index(plan), calls.index(create))

    def test_refusal_and_failed_or_incomplete_plan_never_create(self):
        for extra in ({"STUB_STACK_WHATIF": self.response("Ignore")},
                      {"STUB_STACK_WHATIF": '{"changes": []}'},
                      {"STUB_STACK_WHATIF": "bad json"},
                      {"STUB_STACK_WHATIF_ERROR": "AuthorizationFailed"},
                      {"STUB_RESOURCES_ERROR": "AuthorizationFailed"}):
            r = self.run_guard(**extra)
            self.assertEqual(r.returncode, 42, r.stderr)
        self.assertFalse(any(c.startswith("stack sub create") for c in self.calls()))

    def test_override_and_dry_run(self):
        r = self.run_guard("ALLOW_RENAMES=(--allow-rename=old); deploy_stack",
                           STUB_STACK_WHATIF=self.response("Ignore"))
        self.assertEqual(r.returncode, 0, r.stderr)
        r = self.run_guard("deploy_stack --what-if", STUB_STACK_WHATIF=self.response("Ignore"))
        self.assertEqual(r.returncode, 42, r.stderr)

    def test_new_stack_and_read_errors(self):
        r = self.lib("deploy_stack", STUB_RESOURCES_ERROR="DeploymentStackNotFound: usnm-dev")
        self.assertEqual(r.returncode, 0, r.stderr)
        message = ("ERROR: (ResourceNotFound) The resource 'usnm-dev' was not found. "
                   "For more details please go to https://aka.ms/ARMResourceNotFoundFix\n"
                   "Code: ResourceNotFound\nMessage: The resource 'usnm-dev' was not found.")
        r = self.lib("deploy_stack", STUB_RESOURCES_ERROR=message)
        self.assertEqual(r.returncode, 0, r.stderr)
        for error in ("ResourceNotFound: unexpected dependency",
                      message.replace("usnm-dev", "some-dependency"),
                      message.replace("Code: ResourceNotFound", "Code: AuthorizationFailed")):
            r = self.lib("deploy_stack", STUB_RESOURCES_ERROR=error)
            self.assertEqual(r.returncode, 42, r.stderr)

    def test_guard_failure_is_not_retried(self):
        r = self.lib("vaults_recover() { :; }; settings_store_ensure() { :; }; "
                     "load_store_after_create() { :; }; settings_check() { :; }; "
                     "experiment_marker() { :; }; ocr_nodes_off_check() { :; }; "
                     "deploy_env_guardrails() { :; }; deploy_policy_exemptions() { :; }; "
                     "deploy_stack() { return 42; }; provision")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("not retried", r.stderr)

    def test_cli_rejects_empty_or_wildcard_override_before_azure(self):
        for name in ("", "*", "old,other", "/subscriptions/sub-1/old"):
            r = subprocess.run([os.path.join(HERE, "provision.sh"), "--env", "dev",
                                "--allow-rename=" + name], cwd=ROOT, env=self.env,
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, r.stderr)
        self.assertFalse(os.path.exists(self.log))

    def test_guardrail_users_are_closed_list_plus_archive(self):
        r = self.lib('guardrail_stack_users usnm-dev',
                     STUB_STACK_NAMES="usnm-dev\nusnm-prod\nusnm-xabcd\nusnm-archive\nusnm-guardrails\n"
                     "usnm-config\nusnm-boinc\nusnm-xabcde\nusnm-staging\nother-prod")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["usnm-prod", "usnm-xabcd", "usnm-archive"])
        r = self.lib("guardrail_stack_users", STUB_STACK_LIST_ERROR="AuthorizationFailed")
        self.assertNotEqual(r.returncode, 0)


class TeardownRenameGuard(Stubbed):
    """Real teardown/partial provision/guard; external cleanup is stubbed."""

    GROUP = "/subscriptions/sub-1/resourceGroups/rg-usnm-dev"
    SCRATCH = GROUP + "/providers/Microsoft.Storage/storageAccounts/usnmscratchstdevabc123"
    DNS = GROUP + "/providers/Microsoft.Network/privateDnsZones/privatelink.file.core.windows.net"

    def setUp(self):
        super().setUp()
        self.fixture = os.path.join(self.tmp, "checkout")
        scripts = os.path.join(self.fixture, "scripts")
        azure = os.path.join(scripts, "azure")
        shutil.copytree(os.path.join(SCRIPTS, "lib"), os.path.join(scripts, "lib"))
        os.mkdir(azure)
        shutil.copy(os.path.join(HERE, "teardown.sh"), azure)
        os.mkdir(os.path.join(self.fixture, "infra"))
        shutil.copy(os.path.join(ROOT, "infra", "main.bicepparam"),
                    os.path.join(self.fixture, "infra"))
        # Bypass unrelated preparation only. Retain the real settings,
        # provision retry policy, deploy_stack and removal guard functions.
        with open(os.path.join(scripts, "lib", "env.sh"), "a") as f:
            for name in ("need_stack_az", "vaults_recover", "settings_store_ensure",
                         "load_store_after_create", "settings_check", "experiment_marker",
                         "ocr_nodes_off_check", "deploy_env_guardrails",
                         "deploy_policy_exemptions", "save_outputs"):
                f.write(f"\n{name}() {{ :; }}\n")
        for path, body in (
                (os.path.join(azure, "archive-store.sh"),
                 'printf "archive-store %s\\n" "$*" >> "$STUB_LOG"\n'),
                (os.path.join(self.tmp, "bin", "gh"),
                 'printf "gh %s\\n" "$*" >> "$STUB_LOG"\n'
                 'case "$1 $2" in "auth status"|"api -X") ;; '
                 '"variable get") echo "[]" ;; *) exit 9 ;; esac\n')):
            with open(path, "w") as f:
                f.write("#!/usr/bin/env bash\n" + body)
            os.chmod(path, 0o755)
        vnet = ".".join(str(n) for n in (10, 99, 0, 0)) + "/24"
        self.settings_file(
            'AZURE_SUBSCRIPTION_ID="sub-1"\nUSNM_GITHUB_REPO="owner/repo"\n'
            'USNM_ARCHIVE_ACCOUNT="/subscriptions/sub-1/resourceGroups/rg-usnm-archive/'
            'providers/Microsoft.Storage/storageAccounts/archive"\n'
            'USNM_SEARCH_CLUSTER="true"\nUSNM_SEARCH_LOCAL_MODE="nfs"\n'
            f'USNM_INGEST_SCRATCH_GIB="0"\nUSNM_VNET_ADDRESS_PREFIX="{vnet}"\n')
        self.marker = os.path.join(self.settings, "dev", "teardown-sub-1.archive")

    def teardown(self, *approvals, **extra):
        plan = {"changes": [{"resourceId": rid, "changeType": kind}
                            for rid, kind in ((self.SCRATCH, "Ignore"), (self.DNS, "Delete"))]}
        env = {**self.env, "STUB_STACK": "3", "STUB_STACK_NAMES": "usnm-dev\nusnm-archive",
               "STUB_RESOURCE_IDS": self.GROUP + "\n" + self.SCRATCH + "\n" + self.DNS,
               "STUB_RESOURCES": json.dumps([{"id": rid} for rid in
                                             (self.GROUP, self.SCRATCH, self.DNS)]),
               "STUB_STACK_WHATIF": json.dumps(plan), "STUB_GROUP_EXISTS": "true",
               "STUB_CAPTURE_PARAMS": "1", **extra}
        return subprocess.run(
            ["bash", os.path.join(self.fixture, "scripts", "azure", "teardown.sh"),
             "--env", "dev", *approvals], cwd=self.fixture, env=env,
            input="dev\n", capture_output=True, text=True)

    def test_nfs_partial_provision_requires_every_exact_approval(self):
        r = self.teardown()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("not retried", r.stderr)
        self.assertIn(self.SCRATCH, r.stderr)
        self.assertIn(self.DNS, r.stderr)
        self.assertTrue(os.path.exists(self.marker))
        calls = self.calls()
        self.assertEqual(sum(c.startswith("deployment sub what-if") for c in calls), 1)
        self.assertFalse(any(c.startswith(("stack sub create", "stack sub delete", "group delete"))
                             for c in calls))
        self.assertEqual(calls[-1], "archive-store deploy --subscription sub-1")
        with open(self.log + ".params") as f:
            self.assertEqual(f.read(), "\tfalse\tnfs\t0\n")
        # Resume after the first refusal: one approval still cannot permit
        # another protected removal or final environment deletion.
        r = self.teardown("--allow-rename=" + self.SCRATCH.rsplit("/", 1)[1])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(self.DNS, r.stderr)
        self.assertFalse(any(c.startswith("stack sub create") for c in self.calls()))
        self.assertEqual(self.calls()[-1], "archive-store deploy --subscription sub-1")

    def test_approved_nfs_partial_provision_then_finishes_teardown(self):
        # A refused partial provision has already cleared the archive
        # setting. The saved marker must still allow an approved rerun.
        first = self.teardown()
        self.assertNotEqual(first.returncode, 0)
        self.assertTrue(os.path.exists(self.marker))
        self.assertIn('USNM_ARCHIVE_ACCOUNT=""', self.local_text())
        with open(self.log, "w"):
            pass
        r = self.teardown("--allow-rename=" + self.SCRATCH.rsplit("/", 1)[1],
                          "--allow-rename=privatelink.file.core.windows.net")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        calls = self.calls()
        plan = next(i for i, c in enumerate(calls) if c.startswith("deployment sub what-if"))
        create = next(i for i, c in enumerate(calls) if c.startswith("stack sub create"))
        lock = calls.index("archive-store deploy --subscription sub-1")
        detach = next(i for i, c in enumerate(calls) if c.startswith("stack sub delete"))
        delete = next(i for i, c in enumerate(calls) if c.startswith("group delete"))
        self.assertLess(plan, create)
        self.assertLess(create, lock)
        self.assertLess(lock, detach)
        self.assertLess(detach, delete)
        self.assertIn("gh api -X DELETE repos/owner/repo/environments/dev", calls)
        self.assertFalse(os.path.exists(self.marker))
        self.assertFalse(os.path.exists(os.path.join(self.settings, "dev", ".env")))

    def test_approvals_cannot_bypass_incomplete_or_failed_partial_plan(self):
        for failure in ({"STUB_STACK_WHATIF": '{"changes": []}'},
                        {"STUB_STACK_WHATIF": "bad json"},
                        {"STUB_STACK_WHATIF": json.dumps({"changes": [
                            {"resourceId": self.SCRATCH, "changeType": "Unsupported"},
                            {"resourceId": self.DNS, "changeType": "Delete"}]})},
                        {"STUB_STACK_WHATIF_ERROR": "AuthorizationFailed"},
                        {"STUB_RESOURCES_ERROR": "AuthorizationFailed"}):
            with self.subTest(failure=failure):
                r = self.teardown("--allow-rename=" + self.SCRATCH.rsplit("/", 1)[1],
                                  "--allow-rename=privatelink.file.core.windows.net", **failure)
                self.assertNotEqual(r.returncode, 0)
                self.assertTrue(os.path.exists(self.marker))
                self.assertFalse(any(c.startswith(("stack sub create", "stack sub delete", "group delete"))
                                     for c in self.calls()))
                self.assertEqual(self.calls()[-1], "archive-store deploy --subscription sub-1")

    def test_invalid_approval_rejected_before_external_calls(self):
        for name in ("", "*", "scratch,zone", self.SCRATCH):
            r = self.teardown("--allow-rename=" + name)
            self.assertEqual(r.returncode, 2, r.stderr)
        self.assertFalse(os.path.exists(self.log))


class PolicyExemptions(Stubbed):
    """The public-network policy's assignment, written before the stack (#336)."""

    LAKE = "/subscriptions/sub-1/resourceGroups/rg-usnm-dev/providers/Microsoft.Storage/storageAccounts/stusnmd1"
    TILES = "/subscriptions/sub-1/resourceGroups/rg-usnm-dev/providers/Microsoft.Storage/storageAccounts/stusnmt1"

    def setUp(self):
        super().setUp()
        self.settings_file('AZURE_SUBSCRIPTION_ID="sub-1"\n')

    def test_provision_writes_it_before_the_stack(self):
        r = self.lib("deploy_env_guardrails() { echo guardrails; }; deploy_policy_exemptions() { echo exemptions; }\n"
                     "deploy_stack() { echo stack; }; save_outputs() { :; }\n"
                     "settings_check() { :; }; vaults_recover() { :; }; settings_store_ensure() { :; }\n"
                     "load_store_after_create() { :; }; experiment_marker() { :; }; provision")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.split(), ["guardrails", "exemptions", "stack"])

    def test_nothing_before_the_first_provision(self):
        r = self.lib("deploy_policy_exemptions")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("no policy assignment usnm-audit-data-public-network yet", r.stderr)
        self.assertFalse(any(c.startswith("deployment group") for c in self.calls()))

    def test_writes_the_group_that_holds_it(self):
        r = self.lib("deploy_policy_exemptions", STUB_ASSIGNMENT_GROUP="rg-usnm-dev")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("deployment group create --resource-group rg-usnm-dev --name usnm-dev-policy-exemptions "
                      "--parameters infra/policy-exemptions.bicepparam --only-show-errors -o none", self.calls())

    def test_a_failed_read_stops(self):
        r = self.lib("deploy_policy_exemptions", STUB_ASSIGNMENT_ERROR="ERROR: (AuthorizationFailed) no")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the policy assignment", r.stderr)

    def test_what_if_names_the_ids_it_adds_and_writes_nothing(self):
        after = {"changes": [{"after": {"properties": {"parameters": {
            "publicAccountIds": {"value": [self.TILES, self.LAKE]}}}}}]}
        r = self.lib("deploy_policy_exemptions --what-if", STUB_ASSIGNMENT_GROUP="rg-usnm-dev",
                     STUB_ASSIGNED_IDS=json.dumps([self.TILES]), STUB_EXEMPTIONS_WHATIF=json.dumps(after))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.split(), [self.LAKE])
        self.assertIn("what-if: the policy step", r.stderr)
        self.assertFalse(any(c.startswith("deployment group create") for c in self.calls()))

    REFUSED = ("ERROR: InvalidTemplateDeployment - The template deployment failed because of policy violation.\n"
               "RequestDisallowedByPolicy - Resource '{}' was disallowed by policy. Policy identifiers: '[]'.")

    def pending(self, err, ids):
        return self.lib('only_pending_refusals "$ERR" "$IDS"', ERR=err, IDS=ids).returncode

    def test_a_refusal_of_what_the_step_adds_is_expected(self):
        self.assertEqual(self.pending(self.REFUSED.format("stusnmd1"), self.LAKE), 0)

    def test_any_other_refusal_or_error_fails(self):
        self.assertEqual(self.pending(self.REFUSED.format("stusnmt1"), self.LAKE), 1)
        self.assertEqual(self.pending(self.REFUSED.format("stusnmd1"), ""), 1)
        self.assertEqual(self.pending(self.REFUSED.format("stusnmd1") + "\nERROR: (InvalidTemplate) bad", self.LAKE), 1)
        self.assertEqual(self.pending("ERROR: (AuthorizationFailed) no", self.LAKE), 1)
        self.assertEqual(self.pending(self.REFUSED.format("stusnmd1") + "\nERROR: AuthorizationFailed - no", self.LAKE), 1)

    def test_the_stacks_what_if_accepts_only_those(self):
        script = 'deploy_stack --what-if --exempting "$IDS"'
        # A private block, built from its parts (scripts/ci/no-private-addresses.sh).
        vnet = ".".join(str(n) for n in (10, 99, 0, 0)) + "/24"
        self.settings_file(f'AZURE_SUBSCRIPTION_ID="sub-1"\nUSNM_VNET_ADDRESS_PREFIX="{vnet}"\n')
        r = self.lib(script, IDS=self.LAKE, STUB_STACK="3",
                     STUB_VALIDATE_ERROR=self.REFUSED.format("stusnmd1"))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("preflight refuses only what the policy step exempts first", r.stdout)
        self.assertTrue(any(c.startswith("deployment sub what-if") and "--validation-level Template" in c
                            for c in self.calls()))
        r = self.lib(script, IDS=self.LAKE, STUB_STACK="3",
                     STUB_VALIDATE_ERROR=self.REFUSED.format("stusnmt1"))
        self.assertEqual(r.returncode, 1)


class CommittedSettings(Stubbed):
    """infra/env/<env>.json, the settings store and the local file (#295)."""

    EP = "https://usnm-config-appcs-abc123.azconfig.io"
    STORE = {"SETTINGS_STORE_ENDPOINT": EP}
    OUTPUTS = {"STUB_OUTPUTS": json.dumps({"settingS_STORE_ENDPOINT": {"type": "String", "value": EP}})}

    def read_log(self, suffix):
        path = self.log + suffix
        if not os.path.exists(path):
            return []
        with open(path) as f:
            return f.read().splitlines()

    def store_calls(self):
        if not os.path.exists(self.log):
            return []
        return [c for c in self.calls() if c.startswith("appconfig")]

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
        r = subprocess.run(["python3", os.path.join(SCRIPTS, "lib", "env_settings.py"), "--check"],
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

    def test_private_settings_come_from_the_store(self):
        self.local(**self.STORE)
        r = self.lib("load_store; aget USNM_ALERT_EMAILS; aget USNM_OCR_USERS; aget USNM_OCR_NODE_GROUP",
                     STUB_KEYS="USNM_ALERT_EMAILS\\nUSNM_OCR_USERS",
                     STUB_KV_USNM_ALERT_EMAILS='ops@example.org', STUB_KV_USNM_OCR_USERS='a"b$c`d\\e')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["ops@example.org", 'a"b$c`d\\e', ""])
        # One request reads them all: the Free tier allows 1,000 a day.
        self.assertEqual(self.store_calls(), [
            f"appconfig kv list --endpoint {self.EP} --auth-mode login --label dev --all "
            "--query [].{key: key, value: value} -o json"])

    def test_every_request_names_the_environments_label(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n', env="prod")
        r = self.lib("load_store; aset USNM_OCR_NODE_GROUP g; adel USNM_OCR_USERS; aget USNM_ALERT_EMAILS",
                     env_name="prod")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([c.split()[2] for c in self.read_log(".store")], ["prod"] * 4)

    def test_other_scripts_read_one_key(self):
        # Without load_store (onboard.sh, group.sh, ...): one key, by name.
        self.local(**self.STORE)
        r = self.lib("aget USNM_OCR_NODE_GROUP; aget USNM_OCR_USERS", STUB_KV_USNM_OCR_NODE_GROUP="group-1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["group-1", ""])
        self.assertEqual(self.store_calls()[0],
                         f"appconfig kv list --endpoint {self.EP} --auth-mode login --label dev "
                         "--key USNM_OCR_NODE_GROUP --query [0].value -o tsv")

    def test_an_unreadable_store_fails_the_read(self):
        self.local(**self.STORE)
        with open(os.path.join(self.tmp, "bin", "az"), "w") as f:
            f.write('#!/usr/bin/env bash\necho "ERROR: (Forbidden) not allowed" >&2\nexit 1\n')
        r = self.lib("aget USNM_OCR_NODE_GROUP")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read USNM_OCR_NODE_GROUP", r.stderr)

    def test_no_store_yet_reads_nothing(self):
        # No stack, or a stack without the output: no store.
        for stack in ({}, {"STUB_STACK": "3"}):
            r = self.lib("load_store; aget USNM_ALERT_EMAILS", **stack)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(self.store_calls(), [])

    def test_without_a_local_endpoint_the_store_comes_from_its_stack(self):
        # Another operator's machine: the store's own stack names it.
        self.local(API_URL="x")
        r = self.lib("load_store; aget USNM_ALERT_EMAILS", **self.OUTPUTS,
                     STUB_KEYS="USNM_ALERT_EMAILS", STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "ops@example.org")
        self.assertIn(f'SETTINGS_STORE_ENDPOINT="{self.EP}"', self.local_text())
        self.assertIn("stack sub show -n usnm-config --query outputs -o json", self.calls())

    def test_a_single_read_without_a_local_endpoint_asks_the_stack(self):
        # settings.sh, group.sh, ...: one key, from the store the stack
        # names, which is then recorded.
        self.local(API_URL="x")
        r = self.lib("aget USNM_ALERT_EMAILS; aget USNM_OCR_NODE_GROUP", **self.OUTPUTS,
                     STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["ops@example.org", ""])
        self.assertIn(f'SETTINGS_STORE_ENDPOINT="{self.EP}"', self.local_text())
        # Recorded once: the second read doesn't ask the stack again.
        self.assertEqual(sum(c.startswith("stack sub show") for c in self.calls()), 1)

    def test_the_store_is_looked_up_in_the_environments_subscription(self):
        # Whichever subscription az has selected.
        self.local(AZURE_SUBSCRIPTION_ID="sub-env")
        r = self.lib("aget USNM_ALERT_EMAILS", **self.OUTPUTS)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("stack sub show -n usnm-config --query outputs -o json --subscription sub-env", self.calls())

    def test_a_single_read_with_an_unreadable_stack_fails(self):
        # Not "unset": a script never takes an unreadable setting for an
        # empty one.
        self.local(API_URL="x")
        r = self.lib("aget USNM_ALERT_EMAILS",
                     STUB_OUTPUTS_ERROR="ERROR: (AuthorizationFailed) The client does not have authorization")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read the stack usnm-config's outputs", r.stderr)
        self.assertEqual(self.store_calls(), [])

    def test_a_private_write_without_a_local_endpoint_goes_to_the_store(self):
        self.local(API_URL="x")
        r = self.lib("aset USNM_ALERT_EMAILS ops@example.org", **self.OUTPUTS)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"set USNM_ALERT_EMAILS for dev in the settings store {self.EP}", r.stdout)
        self.assertNotIn("USNM_ALERT_EMAILS=", self.local_text())

    def test_an_operator_without_access_is_told_how_to_get_it(self):
        self.local(**self.STORE)
        r = self.lib("load_store", STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn("App Configuration Data Owner", r.stderr)
        self.assertIn('scripts/azure/settings-store.sh --deploy --admins "<ids>"', r.stderr)
        self.assertIn('az role assignment create --assignee-object-id "$(az ad signed-in-user show', r.stderr)
        self.assertIn("/resourceGroups/usnm-config-rg\"", r.stderr)

    def test_a_store_key_that_isnt_a_private_setting_stops(self):
        self.local(**self.STORE)
        for name in ("USNM_JA_LATIN", "rhel-activation-key"):
            r = self.lib("load_store", STUB_KEYS=name)
            self.assertEqual(r.returncode, 1, name)
            self.assertIn("isn't a private setting", r.stderr)

    def test_a_multi_line_value_stops(self):
        self.local(**self.STORE)
        r = self.lib("load_store", STUB_KEYS="USNM_ALERT_EMAILS", STUB_KV_USNM_ALERT_EMAILS="a\nb")
        self.assertEqual(r.returncode, 1)
        self.assertIn("more than one line", r.stderr)

    def test_a_local_entry_shadows_the_store(self):
        # teardown.sh clears USNM_ARCHIVE_ACCOUNT for its provision.
        self.local(**self.STORE, USNM_ARCHIVE_ACCOUNT="")
        r = self.lib("load_store; settings_check; aget USNM_ARCHIVE_ACCOUNT",
                     STUB_KEYS="USNM_ARCHIVE_ACCOUNT", STUB_KV_USNM_ARCHIVE_ACCOUNT="/subscriptions/x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "")

    def test_a_private_setting_in_two_places_stops(self):
        self.local(**self.STORE, USNM_ALERT_EMAILS="old@example.org")
        r = self.lib("load_store; settings_check",
                     STUB_KEYS="USNM_ALERT_EMAILS", STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_ALERT_EMAILS is in both", r.stderr)
        # Keeping the store's drops only the local copy; --unset would
        # delete it from the store too.
        self.assertIn("--key USNM_ALERT_EMAILS --unset-local", r.stderr)
        self.assertNotIn("--unset ", r.stderr)
        self.assertNotIn("ops@example.org", r.stderr)

    def test_a_private_setting_still_local_is_a_warning_with_the_move(self):
        self.local(**self.STORE, USNM_ALERT_EMAILS="ops@example.org")
        r = self.lib("load_store; settings_check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("USNM_ALERT_EMAILS", r.stderr)
        self.assertIn("scripts/azure/settings.sh --env dev --migrate", r.stderr)
        # The value isn't printed.
        self.assertNotIn("ops@example.org", r.stderr)

    def test_a_public_setting_in_the_local_file_stops(self):
        self.local(USNM_JA_LATIN="true", API_URL="x")
        r = self.lib("settings_check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_JA_LATIN is in", r.stderr)
        self.assertIn("--migrate", r.stderr)

    def test_provision_checks_before_anything_else(self):
        self.settings_file(f'AZURE_SUBSCRIPTION_ID="sub-1"\nSETTINGS_STORE_ENDPOINT="{self.EP}"\n'
                           'USNM_JA_LATIN="true"\n', env="prod")
        r = self.lib("provision", env_name="prod")
        self.assertEqual(r.returncode, 1)
        self.assertIn("not provisioning prod", r.stderr)
        self.assertFalse(os.path.exists(self.log) and any(c.startswith("stack sub create") for c in self.calls()))

    # Provision up to the deployment, which the stub can't answer; the
    # store's script is the stub's too.
    PROVISION = ("deploy_env_guardrails() { :; }; deploy_policy_exemptions() { :; }; deploy_stack() { :; }\n"
                 "save_outputs() { :; }\n"
                 'settings_store_script() { echo "settings-store.sh $*"; }\nprovision')

    def test_the_first_provision_creates_the_store(self):
        # No store in the subscription: its own stack is deployed first, in
        # the environment's region, then read.
        self.local(USNM_EXPERIMENT="295", USNM_ALERT_EMAILS="ops@example.org")
        r = self.lib(self.PROVISION.replace('echo "settings-store.sh $*"',
                                            'echo "settings-store.sh $*"; local_set SETTINGS_STORE_ENDPOINT '
                                            + self.EP))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("settings-store.sh --deploy --location eastus2", r.stdout)
        # The private setting stays local until --migrate, which the warning names.
        self.assertIn("--migrate", r.stderr)
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())

    def test_a_new_stores_role_is_waited_for(self):
        # The new Data Owner assignment takes a while to apply: the first
        # reads are refused, then one works.
        self.local(USNM_EXPERIMENT="295")
        r = self.lib(self.PROVISION.replace('echo "settings-store.sh $*"',
                                            'echo "settings-store.sh $*"; local_set SETTINGS_STORE_ENDPOINT '
                                            + self.EP), STUB_STORE_FAILS="2", STORE_RETRY_SECS="0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.count("may not apply yet"), 2)
        self.assertEqual(len(self.store_calls()), 3)

    def test_an_existing_stores_refusal_fails_at_once(self):
        self.local(**self.STORE, USNM_EXPERIMENT="295")
        r = self.lib(self.PROVISION, STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn("App Configuration Data Owner", r.stderr)
        self.assertEqual(len(self.store_calls()), 1)

    def test_a_store_that_names_no_endpoint_stops_the_provision(self):
        self.local(USNM_EXPERIMENT="295")
        r = self.lib(self.PROVISION)
        self.assertEqual(r.returncode, 1)
        self.assertIn("names no endpoint", r.stderr)

    def test_an_existing_store_isnt_deployed_again(self):
        self.local(**self.STORE, USNM_EXPERIMENT="295", SETTINGS_KEYVAULT="usnmcorekvdevabc123")
        r = self.lib(self.PROVISION)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("settings-store.sh", r.stdout)
        # The settings vault's name from before the store goes.
        self.assertNotIn("SETTINGS_KEYVAULT", self.local_text())

    def test_a_settings_vault_that_still_exists_stops_the_provision(self):
        # Its secrets would go with it: they're copied first.
        self.local(**self.STORE, USNM_EXPERIMENT="295", SETTINGS_KEYVAULT="usnmcorekvdevabc123")
        r = self.lib(self.PROVISION, STUB_LIVE_VAULT="usnmcorekvdevabc123")
        self.assertEqual(r.returncode, 1)
        self.assertIn("the settings vault usnmcorekvdevabc123 still exists", r.stderr)
        self.assertIn('SETTINGS_KEYVAULT="usnmcorekvdevabc123"', self.local_text())
        self.assertEqual(self.store_calls(), [])

    def test_what_if_records_nothing(self):
        # A new checkout: the endpoint comes from the stack and isn't written.
        self.local(USNM_EXPERIMENT="295")
        r = self.lib('deploy_stack() { echo "deploy_stack $*"; }; provision_what_if',
                     STUB_STACK="3", **self.OUTPUTS)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.local_text(), 'USNM_EXPERIMENT="295"\n')
        self.assertIn("deploy_stack --what-if", r.stdout)

    @staticmethod
    def deleted(name, group):
        """A soft-deleted vault as the stub lists it: its name and its id."""
        return f"{name}\\t/subscriptions/sub-1/resourceGroups/{group}/providers/Microsoft.KeyVault/vaults/{name}"

    def test_dev_recovers_the_ocr_vault_into_its_adr_0017_group(self):
        # Dev's project group is usnm-core-rg-dev (infra/naming.bicep), and
        # a teardown deleted it: it's created again, under that name, and
        # the OCR nodes' vault goes back into it.
        self.committed({"USNM_OCR_NODES": True})
        self.local(**self.STORE, USNM_EXPERIMENT="295")
        r = self.lib(self.PROVISION, STUB_DELETED_VAULTS=self.deleted("usnmocrkvdevabc123", "USNM-CORE-RG-DEV"))
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        listed = next(c for c in calls if c.startswith("keyvault list-deleted"))
        # Either name form of the project group (ADR-0017); only the OCR vault.
        self.assertIn("/resourceGroups/rg-usnm-dev/providers/", listed)
        self.assertIn("/resourceGroups/usnm-core-rg-dev/providers/", listed)
        self.assertIn("starts_with(name, 'usnmocrkv')", listed)
        self.assertNotIn("usnmcorekv", listed)
        creates = [c for c in calls if c.startswith("group create")]
        self.assertEqual(creates, ["group create -n usnm-core-rg-dev -l eastus2 --tags project=usnewsmap "
                                   "environment=dev -o none"])
        self.assertLess(calls.index(creates[0]), calls.index("keyvault recover --name usnmocrkvdevabc123 -o none"))

    def test_without_ocr_nodes_no_vault_is_recovered(self):
        self.local(**self.STORE, USNM_EXPERIMENT="295")
        r = self.lib(self.PROVISION)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(any(c.startswith("keyvault") for c in self.calls()))

    def test_teardown_keeps_the_store_and_its_endpoint(self):
        with open(os.path.join(HERE, "teardown.sh")) as f:
            src = f.read()
        self.assertNotIn("SETTINGS_STORE_ENDPOINT", src)
        self.assertNotIn("appconfig", src)
        # The store's stack doesn't keep the guard-rail stack alive.
        self.assertEqual(src.count("$(guardrail_stack_users"), 2)
        r = self.lib("guardrail_stack_users", STUB_STACK_NAMES="usnm-config\nusnm-guardrails")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "")

    def store_sets(self):
        return [c for c in self.calls() if c.startswith("appconfig kv set")]

    def kv_set(self, key, value, env="dev"):
        return (f"appconfig kv set --endpoint {self.EP} --auth-mode login --label {env} --key {key} "
                f"--value {value} --yes -o none")

    def test_a_script_writes_a_private_setting_to_the_store_once_it_exists(self):
        # bootstrap.sh and group.sh set them with aset; a copy left local
        # (from before the store) goes.
        self.local(**self.STORE, USNM_GITHUB_REPO_IDS="o@1/r@2")
        r = self.lib("aset USNM_GITHUB_REPO_IDS o@1/r@3; aset USNM_OCR_NODE_GROUP group-1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.store_sets(), [self.kv_set("USNM_GITHUB_REPO_IDS", "o@1/r@3"),
                                             self.kv_set("USNM_OCR_NODE_GROUP", "group-1")])
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')

    def test_a_bootstrap_rerun_after_migration_leaves_one_copy(self):
        # The values bootstrap.sh sets again are in the store already.
        self.local(**self.STORE, API_URL="x")
        store = {"STUB_KEYS": "USNM_GITHUB_REPO_IDS\\nUSNM_ALERT_EMAILS\\nUSNM_API_CERT_ID",
                 "STUB_KV_USNM_GITHUB_REPO_IDS": "o@1/r@2", "STUB_KV_USNM_ALERT_EMAILS": "ops@example.org",
                 "STUB_KV_USNM_API_CERT_ID": "/subscriptions/x/certificates/api"}
        r = self.lib("aset USNM_GITHUB_REPO_IDS o@1/r@2; aset USNM_ALERT_EMAILS ops@example.org\n"
                     "aset USNM_API_CERT_ID /subscriptions/x/certificates/api; load_store; settings_check", **store)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.store_sets(), [])
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nAPI_URL="x"\n')
        self.assertNotIn("warning", r.stderr)
        # After load_store (provision's): only a changed value is written.
        r = self.lib("load_store; aset USNM_ALERT_EMAILS ops@example.org; aset USNM_ALERT_EMAILS new@example.org",
                     **store)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.store_sets(), [self.kv_set("USNM_ALERT_EMAILS", "new@example.org")])

    def test_an_empty_private_value_stays_local(self):
        # teardown.sh's detach: a local "" shadows the store.
        self.local(**self.STORE)
        r = self.lib("aset USNM_ARCHIVE_ACCOUNT ''")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('USNM_ARCHIVE_ACCOUNT=""', self.local_text())
        self.assertFalse(os.path.exists(self.log) and self.store_sets())

    def test_a_refused_store_write_fails_with_the_fix(self):
        self.local(**self.STORE)
        r = self.lib("aset USNM_ALERT_EMAILS ops@example.org",
                     STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't read USNM_ALERT_EMAILS", r.stderr)
        self.assertIn("az role assignment create", r.stderr)
        self.assertNotIn("USNM_ALERT_EMAILS", self.local_text())

    def test_a_move_brings_the_old_stores_settings_back(self):
        # bootstrap.sh --move: the new subscription has its own store.
        self.local(**self.STORE, USNM_OCR_USERS="kept-local", API_URL="x")
        r = self.lib("store_detach\naset USNM_API_CERT_ID ''; aset USNM_GITHUB_REPO_IDS o@1/r@2\n"
                     "load_store; settings_check",
                     STUB_KEYS="USNM_ALERT_EMAILS\\nUSNM_API_CERT_ID\\nUSNM_OCR_USERS",
                     STUB_KV_USNM_ALERT_EMAILS="ops@example.org", STUB_KV_USNM_OCR_USERS="store",
                     STUB_KV_USNM_API_CERT_ID="/subscriptions/old/certificates/api")
        self.assertEqual(r.returncode, 0, r.stderr)
        text = self.local_text()
        self.assertNotIn("SETTINGS_STORE_ENDPOINT", text)
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', text)
        self.assertIn('USNM_OCR_USERS="kept-local"', text)
        self.assertIn('USNM_API_CERT_ID=""', text)
        self.assertIn('USNM_GITHUB_REPO_IDS="o@1/r@2"', text)
        self.assertEqual(self.store_sets(), [])
        # Once the new store exists, --migrate moves them into it.
        new = "https://usnm-config-appcs-new456.azconfig.io"
        self.settings_file(text + f'SETTINGS_STORE_ENDPOINT="{new}"\n')
        r = self.lib("migrate_settings")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("USNM_GITHUB_REPO_IDS", self.local_text())
        self.assertIn('USNM_API_CERT_ID=""', self.local_text())
        self.assertIn(f"appconfig kv set --endpoint {new} --auth-mode login --label dev --key USNM_GITHUB_REPO_IDS "
                      "--value o@1/r@2 --yes -o none", self.store_sets())

    def test_a_move_from_an_unreadable_store_stops(self):
        self.local(**self.STORE)
        r = self.lib("store_detach", STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn("SETTINGS_STORE_ENDPOINT", self.local_text())

    def test_a_move_reads_the_old_store_in_the_old_subscription(self):
        # bootstrap.sh --move into another tenant: the old store is read
        # with the old subscription selected, then the new one is again.
        self.local(**self.STORE, AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new", STUB_KEYS="USNM_ALERT_EMAILS",
                     STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.read_log(".store"), ["sub-old list dev"])
        self.assertEqual(self.read_log(".account"), ["sub-new"])
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())
        self.assertNotIn("SETTINGS_STORE_ENDPOINT", self.local_text())

    def test_a_move_from_a_tenant_az_isnt_signed_in_to_says_how(self):
        self.local(**self.STORE, AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new", STUB_ACCOUNTS="sub-new")
        self.assertEqual(r.returncode, 1)
        self.assertIn("az login --tenant tenant-old", r.stderr)
        self.assertIn("--subscription sub-new", r.stderr)
        self.assertEqual(self.read_log(".store"), [])
        self.assertIn("SETTINGS_STORE_ENDPOINT", self.local_text())

    def test_a_failed_move_selects_the_new_subscription_again(self):
        self.local(**self.STORE)
        r = self.lib("store_detach_from sub-old sub-new",
                     STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read_log(".account"), ["sub-new"])
        self.assertIn("SETTINGS_STORE_ENDPOINT", self.local_text())

    def test_bootstrap_detaches_before_selecting_the_new_subscription(self):
        with open(os.path.join(HERE, "bootstrap.sh")) as f:
            text = f.read()
        detach = text.index('store_detach_from "$previous" "$SUBSCRIPTION"')
        select = text.index('az account set --subscription "$SUBSCRIPTION"')
        self.assertLess(detach, select)
        self.assertEqual(text.count("az account set"), 1)

    def test_a_move_without_a_local_endpoint_reads_the_old_subscriptions_store(self):
        # Another operator's machine: the old subscription's store stack
        # names the old store, asked with the old subscription selected.
        self.local(AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new", **self.OUTPUTS, STUB_KEYS="USNM_ALERT_EMAILS",
                     STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.read_log(".stack"), ["sub-old"])
        self.assertEqual(self.read_log(".store"), ["sub-old list dev"])
        self.assertEqual(self.read_log(".account"), ["sub-new"])
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())
        self.assertNotIn("SETTINGS_STORE_ENDPOINT", self.local_text())

    def test_a_move_whose_old_stack_cant_be_read_stops(self):
        self.local(AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new",
                     STUB_OUTPUTS_ERROR="ERROR: (AuthorizationFailed) The client does not have authorization")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.read_log(".stack"), ["sub-old"])
        self.assertEqual(self.read_log(".store"), [])
        self.assertEqual(self.read_log(".account"), ["sub-new"])

    def test_a_move_from_a_subscription_without_a_store_copies_nothing(self):
        self.local(AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.read_log(".stack"), ["sub-old"])
        self.assertEqual(self.read_log(".store"), [])
        self.assertEqual(self.read_log(".account"), ["sub-new"])

    def test_a_move_without_a_local_endpoint_needs_the_old_subscription(self):
        # Without its endpoint, only the old stack can say whether there's a
        # store, so a subscription az can't select stops the move.
        self.local(AZURE_SUBSCRIPTION_ID="sub-old", AZURE_TENANT_ID="tenant-old")
        r = self.lib("store_detach_from sub-old sub-new", STUB_ACCOUNTS="sub-new")
        self.assertEqual(r.returncode, 1)
        self.assertIn("az login --tenant tenant-old", r.stderr)
        self.assertEqual(self.read_log(".stack"), [])

    def test_an_empty_endpoint_moves_without_the_old_store(self):
        # An old subscription that's gone: SETTINGS_STORE_ENDPOINT="" says so.
        self.local(AZURE_SUBSCRIPTION_ID="sub-old", SETTINGS_STORE_ENDPOINT="")
        r = self.lib("store_detach_from sub-old sub-new", STUB_ACCOUNTS="sub-new")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(self.log) and
                         any(c.startswith(("account set", "stack", "appconfig")) for c in self.calls()))
        self.assertEqual(self.local_text(), 'AZURE_SUBSCRIPTION_ID="sub-old"\n')

    def test_unsetting_a_private_setting_deletes_it_from_the_store(self):
        # --unset: the setting takes its default, not the store's value.
        self.local(**self.STORE, USNM_ALERT_EMAILS="old@example.org")
        r = self.lib('adel USNM_ALERT_EMAILS', STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"appconfig kv delete --endpoint {self.EP} --auth-mode login --label dev "
                      "--key USNM_ALERT_EMAILS --yes -o none", self.calls())
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')

    def test_unsetting_a_key_the_store_lacks(self):
        self.local(**self.STORE, USNM_ALERT_EMAILS="")
        r = self.lib("adel USNM_ALERT_EMAILS",
                     STUB_STORE_ERROR="ERROR: Key 'USNM_ALERT_EMAILS' with label 'dev' does not exist.")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')

    def test_unsetting_before_the_store_exists_is_local(self):
        self.local(USNM_ALERT_EMAILS="ops@example.org")
        r = self.lib("adel USNM_ALERT_EMAILS")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.local_text(), "")
        self.assertEqual(self.store_calls(), [])

    def test_a_refused_delete_keeps_the_local_copy(self):
        self.local(**self.STORE, USNM_ALERT_EMAILS="old@example.org")
        r = self.lib("adel USNM_ALERT_EMAILS",
                     STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn("can't delete USNM_ALERT_EMAILS", r.stderr)
        self.assertIn("az role assignment create", r.stderr)
        self.assertIn('USNM_ALERT_EMAILS="old@example.org"', self.local_text())

    def test_unset_then_set_again(self):
        # The Free tier keeps no deleted keys: no recovery step.
        self.local(**self.STORE)
        r = self.lib("adel USNM_ALERT_EMAILS; aset USNM_ALERT_EMAILS new@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([c.split()[1] for c in self.read_log(".store")], ["delete", "list", "set"])

    def test_migrate_moves_the_public_settings(self):
        self.local(USNM_JA_LATIN="true", USNM_INGEST_SCRATCH_GIB="64", USNM_API_IMAGE="",
                   USNM_ALERT_EMAILS="ops@example.org", API_URL="x")
        self.committed({"USNM_INGEST_SCRATCH_GIB": 64})
        r = self.lib("migrate_settings; settings_check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data(), {"$schema": "./schema.json", "USNM_INGEST_SCRATCH_GIB": 64,
                                                 "USNM_JA_LATIN": True})
        self.assertEqual(self.local_text(), 'USNM_ALERT_EMAILS="ops@example.org"\nAPI_URL="x"\n')
        # No store yet: the private one stays.
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

    def test_migrate_moves_the_private_settings_once_the_store_exists(self):
        # prod's four, as the operator's file holds them; an empty one
        # shadows the store on purpose and stays.
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_OCR_NODE_IPS="203.0.113.7"\n'
                           'USNM_OCR_NODE_GROUP="group-1"\nUSNM_OCR_KEYVAULT_ADMINS="id-1,id-2"\n'
                           'USNM_VNET_ADDRESS_PREFIX="198.51.100.0/24"\nUSNM_ARCHIVE_ACCOUNT=""\n'
                           'API_URL="x"\n', env="prod")
        # One of them is in the store already, with the same value.
        r = self.lib("migrate_settings", env_name="prod", STUB_KEYS="USNM_OCR_NODE_GROUP",
                     STUB_KV_USNM_OCR_NODE_GROUP="group-1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.store_sets(), [self.kv_set("USNM_OCR_NODE_IPS", "203.0.113.7", "prod"),
                                             self.kv_set("USNM_OCR_KEYVAULT_ADMINS", "id-1,id-2", "prod"),
                                             self.kv_set("USNM_VNET_ADDRESS_PREFIX", "198.51.100.0/24", "prod")])
        self.assertEqual(self.local_text("prod"),
                         f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_ARCHIVE_ACCOUNT=""\nAPI_URL="x"\n')
        self.assertIn("moved 4 private settings", r.stdout)
        self.assertIn("kept USNM_ARCHIVE_ACCOUNT=\"\"", r.stdout)

    def test_migrate_with_another_value_in_the_store_moves_no_private_one(self):
        self.local(**self.STORE, USNM_OCR_NODE_GROUP="group-1", USNM_ALERT_EMAILS="ops@example.org")
        r = self.lib("migrate_settings", STUB_KEYS="USNM_OCR_NODE_GROUP", STUB_KV_USNM_OCR_NODE_GROUP="group-2")
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_OCR_NODE_GROUP", r.stderr)
        self.assertNotIn("group-", r.stderr)
        self.assertEqual(self.store_sets(), [])
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())

    def test_migrate_names_the_old_admin_list(self):
        self.local(USNM_SETTINGS_VAULT_ADMINS="id-1")
        r = self.lib("migrate_settings")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("scripts/azure/settings-store.sh --deploy --admins", r.stdout)
        self.assertIn('USNM_SETTINGS_VAULT_ADMINS="id-1"', self.local_text())

    def test_what_if_validates_and_deploys_nothing(self):
        self.local(**self.STORE, USNM_EXPERIMENT="295")
        r = self.lib('deploy_stack() { echo "deploy_stack $*"; }; provision_what_if', STUB_STACK="3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("deploy_stack --what-if", r.stdout)
        self.assertNotIn("settings-store", r.stdout)
        self.assertFalse(any(c.startswith(("stack sub create", "appconfig kv set", "keyvault recover"))
                             for c in self.calls()))

    def test_what_if_without_a_store_shows_the_stores_too(self):
        self.local(USNM_ALERT_EMAILS="ops@example.org", USNM_EXPERIMENT="295")
        r = self.lib('deploy_stack() { echo "deploy_stack $*"; }\n'
                     'settings_store_script() { echo "settings-store.sh $*"; }; provision_what_if', STUB_STACK="3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("settings-store.sh --what-if --location eastus2", r.stdout)
        self.assertIn("deploy_stack --what-if", r.stdout)

    def test_what_if_checks_devs_experiment_and_writes_nothing(self):
        self.local(**self.STORE)
        r = self.lib('deploy_stack() { echo "deploy_stack $*"; }; provision_what_if', STUB_STACK="3")
        self.assertEqual(r.returncode, 1)
        self.assertIn("dev exists only during an experiment", r.stderr)
        # With one: the start goes to the deployment, not the local file.
        self.local(**self.STORE, USNM_EXPERIMENT="295", USNM_VNET_ADDRESS_PREFIX=self.vnet())
        r = self.lib("provision_what_if; echo \"started=$EXPERIMENT_STARTED_DRY_RUN\"", STUB_STACK="3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout, r"started=\d{4}-\d\d-\d\dT")
        self.assertNotIn("USNM_EXPERIMENT_STARTED", self.local_text())
        self.assertTrue(any(c.startswith("stack sub validate") for c in self.calls()))

    def test_what_if_without_the_guardrail_stack_shows_its_changes(self):
        # A new subscription: the stack's preflight needs the definitions
        # the provision deploys first.
        self.local(**self.STORE, USNM_EXPERIMENT="295")
        r = self.lib('deploy_stack() { echo "deploy_stack $*"; }; provision_what_if')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("no guard-rail stack usnm-guardrails yet", r.stdout)
        self.assertIn("deployment sub what-if --location eastus2 --template-file infra/guardrails.bicep --only-show-errors",
                      self.calls())
        self.assertNotIn("deploy_stack", r.stdout)

    def vnet(self):
        # A private block, built from its parts (no private address is
        # written out: scripts/ci/no-private-addresses.sh).
        return ".".join(str(n) for n in (10, 99, 0, 0)) + "/24"

    def test_deploy_stack_what_if_runs_the_preflight_and_creates_nothing(self):
        self.local(**self.STORE, USNM_VNET_ADDRESS_PREFIX=self.vnet())
        r = self.lib("deploy_stack --what-if")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        validate = next(c for c in calls if c.startswith("stack sub validate"))
        self.assertIn("--name usnm-dev", validate)
        self.assertIn("--deny-settings-mode denyDelete", validate)
        preview = next(c for c in calls if c.startswith("stack-whatif sub create"))
        self.assertLess(calls.index(validate), calls.index(preview))
        self.assertIn("--subscription sub-1", preview)
        self.assertIn("/subscriptions/sub-1/providers/Microsoft.Resources/deploymentStacks/usnm-dev", preview)
        self.assertIn("--retention-interval PT3H", preview)
        self.assertIn("--action-on-unmanage deleteResources", preview)
        self.assertIn("--deny-settings-mode denyDelete", preview)
        self.assertFalse(any(c.startswith("stack sub create") for c in calls))

    def test_a_policy_denial_fails_the_what_if(self):
        self.local(**self.STORE, USNM_VNET_ADDRESS_PREFIX=self.vnet())
        r = self.lib("deploy_stack --what-if",
                     STUB_VALIDATE_ERROR="ERROR: (RequestDisallowedByPolicy) Resource was disallowed by policy.")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("RequestDisallowedByPolicy", r.stderr)
        self.assertFalse(any(c.startswith("deployment sub what-if") for c in self.calls()))


class SettingsScript(Stubbed):
    EP = "https://usnm-config-appcs-abc123.azconfig.io"

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

    def test_a_private_setting_with_a_store_goes_there(self):
        # A local copy (from before the store) goes, so it can't hide the
        # store's value and stop the provision.
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_ALERT_EMAILS="old@example.org"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("scripts/azure/provision.sh --env dev", r.stdout)
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')
        self.assertIn(f"appconfig kv set --endpoint {self.EP} --auth-mode login --label dev --key USNM_ALERT_EMAILS "
                      "--value ops@example.org --yes -o none", self.calls())
        r = self.lib("load_store; settings_check", STUB_KEYS="USNM_ALERT_EMAILS",
                     STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("USNM_ALERT_EMAILS", r.stderr)

    def test_a_refused_store_write_keeps_the_local_copy(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_ALERT_EMAILS="old@example.org"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org",
                              STUB_STORE_ERROR="ERROR: Operation returned an invalid status 'Forbidden'")
        self.assertEqual(r.returncode, 1)
        self.assertIn('USNM_ALERT_EMAILS="old@example.org"', self.local_text())

    def test_unset_clears_a_private_setting_in_the_store(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--unset")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"appconfig kv delete --endpoint {self.EP} --auth-mode login --label dev "
                      "--key USNM_ALERT_EMAILS --yes -o none", self.calls())
        self.assertIn(f"cleared USNM_ALERT_EMAILS in the settings store {self.EP}", r.stdout)
        self.assertIn("scripts/azure/provision.sh --env dev", r.stdout)

    def test_a_new_machine_reports_the_stacks_store(self):
        # No local endpoint: the store's stack names it before the message.
        outputs = json.dumps({"SETTINGS_STORE_ENDPOINT": {"type": "String", "value": self.EP}})
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org",
                              STUB_OUTPUTS=outputs)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"USNM_ALERT_EMAILS is in the settings store {self.EP} (label dev)", r.stdout)
        self.assertNotIn("until the settings store exists", r.stdout)
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')
        os.remove(os.path.join(self.settings, "dev", ".env"))
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "", STUB_OUTPUTS=outputs)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("hides the settings store's value", r.stdout)
        os.remove(os.path.join(self.settings, "dev", ".env"))
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--unset", STUB_OUTPUTS=outputs)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"cleared USNM_ALERT_EMAILS in the settings store {self.EP}", r.stdout)
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')

    def test_unset_before_the_store_exists(self):
        self.settings_file('USNM_ALERT_EMAILS="ops@example.org"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--unset")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("cleared USNM_ALERT_EMAILS in", r.stdout)
        self.assertNotIn("settings store", r.stdout)
        self.assertFalse(any(c.startswith("appconfig") for c in self.calls()))

    def test_unset_local_keeps_the_stores_value(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_GITHUB_REPO_IDS="o@1/r@1"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_GITHUB_REPO_IDS", "--unset-local",
                              STUB_KV_USNM_GITHUB_REPO_IDS="o@1/r@2")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("now comes from the settings store", r.stdout)
        self.assertEqual(self.local_text(), f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')
        self.assertFalse(any(c.startswith(("appconfig kv delete", "appconfig kv set")) for c in self.calls()))

    def test_one_change_at_a_time(self):
        r = self.run_settings("--env", "dev", "--key", "USNM_OCR", "--unset", "--unset-local")
        self.assertEqual(r.returncode, 2)

    def test_a_private_setting_is_read_from_the_store(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\n')
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS",
                              STUB_KV_USNM_ALERT_EMAILS="ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, "ops@example.org\n")

    def test_a_private_setting_without_a_store_stays_local(self):
        r = self.run_settings("--env", "dev", "--key", "USNM_ALERT_EMAILS", "--value", "ops@example.org")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('USNM_ALERT_EMAILS="ops@example.org"', self.local_text())
        self.assertIn("--migrate", r.stdout)

    def test_migrate(self):
        self.settings_file('USNM_OCR="true"\n')
        r = self.run_settings("--env", "dev", "--migrate")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.committed_data()["USNM_OCR"], True)
        self.assertIn("moved 1", r.stdout)
        self.assertIn("pull request", r.stdout)

    def test_migrate_of_private_settings_only_has_nothing_to_commit(self):
        self.settings_file(f'SETTINGS_STORE_ENDPOINT="{self.EP}"\nUSNM_OCR_NODE_GROUP="group-1"\n')
        r = self.run_settings("--env", "dev", "--migrate")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("moved 1 private settings", r.stdout)
        self.assertNotIn("pull request", r.stdout)

    def test_migrate_takes_only_the_environment(self):
        r = self.run_settings("--env", "dev", "--migrate", "--key", "USNM_OCR")
        self.assertEqual(r.returncode, 2)


class ProvisionScript(Stubbed):
    def provision(self, *args):
        return subprocess.run([os.path.join(HERE, "provision.sh"), "--env", "dev", *args], cwd=ROOT,
                              env=self.env, capture_output=True, text=True)

    def test_index_storage_serve_requires_its_account_before_azure(self):
        self.committed({"USNM_INDEX_STORAGE_SERVE": True})
        r = self.provision()
        self.assertEqual(r.returncode, 1)
        self.assertIn("USNM_INDEX_STORAGE_SERVE requires USNM_INDEX_STORAGE", r.stderr)
        self.assertFalse(os.path.exists(self.log))

    def test_index_storage_serve_with_its_account_passes_the_guard(self):
        self.committed({"USNM_INDEX_STORAGE": True, "USNM_INDEX_STORAGE_SERVE": True})
        r = self.provision()
        self.assertNotIn("requires USNM_INDEX_STORAGE", r.stderr)
        self.assertTrue(os.path.exists(self.log))

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
        return subprocess.run(["python3", os.path.join(SCRIPTS, "lib", "env_settings.py"), "--check"],
                              env={**os.environ, "USNM_ENV_DIR": self.dir}, capture_output=True, text=True)

    def test_the_checkouts_files(self):
        r = subprocess.run(["python3", os.path.join(SCRIPTS, "lib", "env_settings.py"), "--check"],
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

    def test_a_file_for_a_name_outside_adr_0017s_list_fails(self):
        # scripts/lib/env-name.sh's rule: prod, dev or x plus 1-4.
        for name in ("foo", "xabcde", "x", "Prod", "dev2"):
            with self.subTest(name=name):
                r = self.check("{}\n", name=f"{name}.json")
                self.assertEqual(r.returncode, 1)
                self.assertIn("not an environment name", r.stderr)
                os.remove(os.path.join(self.dir, f"{name}.json"))
        r = self.check("{}\n", name="x1a2.json")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_env_refuses_a_name_outside_adr_0017s_list(self):
        for name in ("foo", "xabcde", "dev\n"):
            with self.subTest(name=name):
                r = subprocess.run(["python3", os.path.join(SCRIPTS, "lib", "env_settings.py"), "--env", name, "--shell"],
                                   env={**os.environ, "USNM_ENV_DIR": self.dir}, capture_output=True, text=True)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("invalid environment name", r.stderr)


if __name__ == "__main__":
    unittest.main()
