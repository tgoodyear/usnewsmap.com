"""The split checks must preserve branch protection and publishing dependencies."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
CI = (ROOT / ".github/workflows/ci.yml").read_text()


def job(name):
    match = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [a-z0-9-]+:\n|\Z)", CI)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match[1]


class Gates(unittest.TestCase):
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
        self.assertIn("suite: [parity, pipeline]", job("quickwit-tests"))
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

    def test_script_shards_install_the_pinned_arm_contract_compiler(self):
        scripts = job("script-tests")
        self.assertLess(scripts.index("run: scripts/ci/install-bicep.sh"),
                        scripts.index("run: scripts/ci/run-unit-tests.py"))
