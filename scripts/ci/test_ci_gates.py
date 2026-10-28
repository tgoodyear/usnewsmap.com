"""The split checks must preserve branch protection and publishing dependencies."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
CI = (ROOT / ".github/workflows/ci.yml").read_text()


def job(name):
    match = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [a-z0-9-]+:\n|\Z)", CI)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match[1]


class Gates(unittest.TestCase):
    def worker_gate(self, outputs=None, results=None, event="pull_request"):
        text = job("worker-contrib")
        flags = dict.fromkeys(("contrib", "boinc_pages", "ocr_worker", "ocr_dash"), "false")
        flags.update(outputs or {})
        names = re.findall(r"^      - ([a-z0-9-]+)$", text, re.M)
        needs = {name: {"result": "skipped"} for name in names}
        needs["changes"] = {"result": "success", "outputs": flags}
        for name, result in (results or {}).items():
            if result is None:
                needs.pop(name, None)
            else:
                needs[name] = {"result": result}
        env = dict(os.environ, GITHUB_EVENT_NAME=event, NEEDS_JSON=json.dumps(needs))
        for name, output in re.findall(
            r"^          ([A-Z_]+): \$\{\{ needs\.changes\.outputs\.([a-z_]+) \}\}$",
            text, re.M,
        ):
            env[name] = flags[output]
        script = re.search(
            r"(?ms)^          python3 - <<'PY'\n(.*?)^          PY$", text,
        )[1]
        return subprocess.run(
            [sys.executable, "-c", textwrap.dedent(script)],
            env=env, text=True, capture_output=True,
        )

    def test_worker_gate_reports_success_when_all_path_scoped_jobs_are_skipped(self):
        self.assertIn("    if: always()\n", job("worker-contrib"))
        result = self.worker_gate()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_worker_gate_accepts_successful_selected_checks(self):
        contrib = {name: "success" for name in (
            "contrib-compute", "boinc-db-tls", "boinc-mysql84-db-tls",
            "boinc-node-client", "contrib-images",
        )}
        cases = [
            ({"contrib": "true"}, contrib, "pull_request"),
            ({"boinc_pages": "true"}, {"boinc-pages": "success"}, "pull_request"),
            ({"ocr_worker": "true"}, {
                "ocr-worker-el9": "success", "ocr-worker-check": "success",
            }, "pull_request"),
            ({"ocr_dash": "true"}, {"ocr-worker-el9": "success"}, "pull_request"),
        ]
        for event in ("push", "workflow_dispatch"):
            cases.append(({"ocr_dash": "true"}, {
                "ocr-worker-el9": "success", "ocr-worker-check": "success",
            }, event))
        for flags, results, event in cases:
            with self.subTest(flags=flags, event=event):
                result = self.worker_gate(flags, results, event)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_worker_gate_fails_closed_for_each_selected_child(self):
        children = (
            "contrib-compute", "boinc-pages", "boinc-db-tls", "boinc-mysql84-db-tls",
            "boinc-node-client", "contrib-images", "ocr-worker-el9", "ocr-worker-check",
        )
        flags = dict.fromkeys(("contrib", "boinc_pages", "ocr_worker", "ocr_dash"), "true")
        for child in children:
            for state in ("failure", "cancelled", "skipped", None):
                with self.subTest(child=child, state=state):
                    results = dict.fromkeys(children, "success")
                    results[child] = state
                    result = self.worker_gate(flags, results)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(child, result.stderr)

    def test_worker_gate_rejects_bad_classification_or_unexpected_results(self):
        for state in ("failure", "cancelled", "skipped", None):
            with self.subTest(changes=state):
                result = self.worker_gate(results={"changes": state})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("changes must succeed", result.stderr)
        for value in ("", "True", "unknown"):
            with self.subTest(flag=value):
                result = self.worker_gate(outputs={"contrib": value})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("CONTRIB_REQUIRED", result.stderr)
        for state in ("failure", "cancelled", "success", None):
            with self.subTest(unselected=state):
                result = self.worker_gate(results={"boinc-pages": state})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("boinc-pages", result.stderr)
        result = self.worker_gate(event="unknown")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported CI event", result.stderr)

    def test_required_gates_fail_closed_on_failed_cancelled_or_skipped_children(self):
        for gate, child in [("quickwit", "quickwit-tests"), ("image", "image-tests")]:
            text = job(gate)
            self.assertIn(f"needs: [changes, {child}]", text)
            self.assertIn("if: always()", text)
            self.assertIn(f"RESULT: ${{{{ needs.{child}.result }}}}", text)
            self.assertIn('test "$RESULT" = success', text)
        infra = job("infra")
        self.assertIn("needs: [changes, infra-checks, script-tests]", infra)
        self.assertIn('test "$CONFIG_RESULT" = success', infra)
        self.assertIn('test "$SCRIPTS_RESULT" = success', infra)
        self.assertIn('test "$SCRIPTS_RESULT" = skipped', infra)

    def test_suites_are_isolated_and_publication_still_waits_for_aggregates(self):
        self.assertIn("suite: ${{ fromJSON(needs.changes.outputs.quickwit_suites) }}",
                      job("quickwit-tests"))
        self.assertIn("if: matrix.suite == 'parity'", job("quickwit-tests"))
        self.assertIn("image: [api, ingest, ja-ocr]", job("image-tests"))
        self.assertIn("shard: [0, 1, 2]", job("script-tests"))
        for name in ("quickwit-tests", "image-tests", "script-tests",
                     "quickwit", "image", "infra-checks", "infra"):
            self.assertNotIn("continue-on-error", job(name), name)
        needs = re.search(r"needs: \[(.*)\]", job("publish"))[1].split(", ")
        self.assertTrue({"quickwit", "image", "infra", "rust"} <= set(needs))

    def test_logged_test_commands_use_explicit_bash_pipefail(self):
        for name in ("rust", "quickwit-tests"):
            for step in re.split(r"(?m)^      - ", job(name))[1:]:
                if "| tee " in step:
                    self.assertIn("shell: bash", step)

    def test_indexnow_notifications_cannot_replace_pending_deployments(self):
        publish = job("publish")
        notify = job("indexnow")
        scheduled = (ROOT / ".github/workflows/indexnow.yml").read_text()

        def group(text):
            return re.search(r"(?m)^\s+group: (.+)$", text)[1].replace(
                "${{ matrix.environment }}", "prod",
            )

        self.assertNotIn("scripts/ci/indexnow.sh", publish)
        self.assertIn("needs: publish", notify)
        self.assertIn("needs.publish.result == 'success'", notify)
        self.assertIn("scripts/ci/indexnow.sh", notify)
        self.assertIn("scripts/ci/indexnow.sh", scheduled)
        self.assertEqual(group(notify), group(scheduled))
        self.assertNotEqual(group(publish), group(scheduled))

    def test_script_shards_install_the_pinned_arm_contract_compiler(self):
        scripts = job("script-tests")
        self.assertLess(scripts.index("run: scripts/ci/install-bicep.sh"),
                        scripts.index("run: scripts/ci/run-unit-tests.py"))
