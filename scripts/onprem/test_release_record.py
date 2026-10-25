"""Tests for release.sh's record of a published run in Cosmos (#1096, ADR-0027 settled decision 5):
python3 -m unittest -v scripts/onprem/test_release_record.py. Needs jq.

usnm-ingest is a stub that logs each call's arguments and the two settings it reads from the
environment, so nothing here reaches Azure. The run directory holds only the staged pointer.

release.sh expands empty arrays under `set -u` (seed's `every`, publish's `dry`), which bash
before 4.4 refuses, so the tests that get as far as that call skip on the macOS system bash
(CI and the release container run Linux)."""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "release.sh")
LAKE = "stexampletest"
INDEX_ACCOUNT = "stexampleidx"
VERSION = "pages-v20261024-1"
ENDPOINT = "https://cosmos-example.documents.azure.com:443/"

STUB = r'''#!PYTHON
import json, os, sys
with open(os.environ["STUB_LOG"], "a") as log:
    seen = {k: os.environ[k] for k in ("USNM_COSMOS_ENDPOINT", "USNM_STATE_FILE") if k in os.environ}
    log.write(json.dumps({"argv": sys.argv[1:], "env": seen}) + "\n")
fail = os.environ.get("STUB_FAIL", "")
if fail and fail in sys.argv[1:]:
    print("stub: %s failed" % fail, file=sys.stderr)
    sys.exit(1)
'''


def empty_arrays_work():
    """Whether this machine's bash expands an empty array under `set -u`."""
    done = subprocess.run(["bash", "-c", 'set -u; a=(); echo "${a[@]}"'], capture_output=True)
    return done.returncode == 0


NEEDS_NEW_BASH = unittest.skipUnless(empty_arrays_work(), "release.sh needs bash 4.4+ (empty arrays under set -u)")


class ReleaseRecordTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.run_dir = os.path.join(self.tmp.name, "run")
        self.reference = os.path.join(self.run_dir, "reference")
        os.makedirs(self.reference)
        self.log = os.path.join(self.tmp.name, "calls.jsonl")
        self.stub = os.path.join(self.tmp.name, "usnm-ingest")
        with open(self.stub, "w") as f:
            f.write(STUB.replace("PYTHON", sys.executable, 1))
        os.chmod(self.stub, 0o755)
        self.pointer({"index_version": VERSION, "previous_version": "pages-v20261010-2"})
        with open(os.path.join(self.reference, "current.json"), "w") as f:
            json.dump({"index_version": "pages-v20261010-2"}, f)

    def pointer(self, doc):
        with open(os.path.join(self.reference, "index-storage-pending.json"), "w") as f:
            f.write(doc if isinstance(doc, str) else json.dumps(doc))

    def run_script(self, *args, env=None):
        base = {k: v for k, v in os.environ.items() if not k.startswith(("USNM_", "STUB_"))}
        base.update(USNM_INGEST=self.stub, USNM_LAKE=LAKE, USNM_INDEX_ACCOUNT=INDEX_ACCOUNT, STUB_LOG=self.log)
        base.update(env or {})
        return subprocess.run([SCRIPT, "--run-dir", self.run_dir, *args], env=base, text=True, capture_output=True)

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return [json.loads(line) for line in f]

    def lake_curated(self):
        return "https://%s.blob.core.windows.net/curated" % LAKE

    @NEEDS_NEW_BASH
    def test_publish_records_the_run_after_the_copy(self):
        done = self.run_script("--step", "publish", "--cosmos-endpoint", ENDPOINT)
        self.assertEqual(done.returncode, 0, done.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        copy, record = calls
        self.assertEqual(
            copy["argv"],
            ["--state-file", self.run_dir + "/state.json", "--curated", self.lake_curated(),
             "--reference", self.reference, "publish-staged", "--local-index-root", self.run_dir + "/qw",
             "--to-reference", "https://%s.blob.core.windows.net/reference" % LAKE,
             "--to-index", "https://%s.blob.core.windows.net/qw-index" % INDEX_ACCOUNT],
        )
        # Cosmos only: no state file, and no --current (the host has no role on ops).
        self.assertEqual(
            record["argv"],
            ["--cosmos", ENDPOINT, "--curated", self.lake_curated(), "--reference", self.reference,
             "import-staged-run", "--version", VERSION],
        )

    @NEEDS_NEW_BASH
    def test_the_endpoint_may_come_from_the_environment_and_the_flag_wins(self):
        done = self.run_script("--step", "publish", env={"USNM_COSMOS_ENDPOINT": ENDPOINT})
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.calls()[1]["argv"][:2], ["--cosmos", ENDPOINT])
        os.remove(self.log)
        other = "https://cosmos-other.documents.azure.com/"
        done = self.run_script("--step", "publish", "--cosmos-endpoint", other, env={"USNM_COSMOS_ENDPOINT": ENDPOINT})
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.calls()[1]["argv"][:2], ["--cosmos", other])

    @NEEDS_NEW_BASH
    def test_the_endpoint_forms_of_a_cosmos_account_are_accepted(self):
        for endpoint in (
            "https://acct.documents.azure.com/",
            "https://acct.documents.azure.com:443/",
            "https://acct.documents.azure.com",
            "https://usnm-cosmos-1.documents.azure.com:443",
        ):
            with self.subTest(endpoint=endpoint):
                if os.path.exists(self.log):
                    os.remove(self.log)
                done = self.run_script("--step", "publish", "--cosmos-endpoint", endpoint)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertEqual(self.calls()[1]["argv"][:2], ["--cosmos", endpoint])

    @NEEDS_NEW_BASH
    def test_neither_call_gets_the_others_setting_from_the_environment(self):
        # usnm-ingest refuses --cosmos with a state file, and reads both from these variables.
        env = {"USNM_COSMOS_ENDPOINT": ENDPOINT, "USNM_STATE_FILE": "/elsewhere/state.json"}
        done = self.run_script("--step", "publish", env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        for call in self.calls():
            self.assertEqual(call["env"], {}, call["argv"])
        os.remove(self.log)
        done = self.run_script("--step", "seed", env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 1)
        self.assertIn("seed-state", calls[0]["argv"])
        self.assertEqual(calls[0]["env"], {})

    def test_a_dry_run_records_nothing_and_needs_no_endpoint(self):
        for extra in ([], ["--cosmos-endpoint", ENDPOINT]):
            with self.subTest(extra=extra):
                if os.path.exists(self.log):
                    os.remove(self.log)
                done = self.run_script("--step", "publish", "--dry-run", *extra)
                self.assertEqual(done.returncode, 0, done.stderr)
                calls = self.calls()
                self.assertEqual(len(calls), 1)
                self.assertIn("publish-staged", calls[0]["argv"])
                self.assertEqual(calls[0]["argv"][-1], "--dry-run")
                self.assertNotIn("import-staged-run", calls[0]["argv"])

    def test_a_missing_endpoint_stops_before_the_copy(self):
        done = self.run_script("--step", "publish")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("--cosmos-endpoint", done.stderr)
        self.assertEqual(self.calls(), [])
        done = self.run_script("--step", "publish", "--cosmos-endpoint", "")
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_an_endpoint_that_is_not_a_cosmos_account_stops_before_the_copy(self):
        for endpoint in (
            "http://acct.documents.azure.com/",
            "https://acct.documents.azure.com.example.net/",
            "https://example.net/acct.documents.azure.com",
            "https://acct.documents.azure.com@example.net/",
            "https://acct.documents.azure.com:8443/",
            "https://acct.documents.azure.com/extra",
            "https://acct.table.core.windows.net/",
            "https://.documents.azure.com/",
            "https://Acct.documents.azure.com/",
            "http://127.0.0.1:10030/",
            "acct",
        ):
            for how in ("flag", "environment"):
                with self.subTest(endpoint=endpoint, how=how):
                    if how == "flag":
                        done = self.run_script("--step", "publish", "--cosmos-endpoint", endpoint)
                    else:
                        done = self.run_script("--step", "publish", env={"USNM_COSMOS_ENDPOINT": endpoint})
                    self.assertNotEqual(done.returncode, 0)
                    self.assertIn("isn't https://<account>.documents.azure.com/", done.stderr)
                    self.assertEqual(self.calls(), [])

    def test_a_missing_or_unusable_pointer_stops_before_the_copy(self):
        os.remove(os.path.join(self.reference, "index-storage-pending.json"))
        cases = [(None, "index-storage-pending.json")]
        cases += [(doc, text) for doc, text in (
            ("not json", "isn't JSON"),
            ({"previous_version": "pages-v20261010-2"}, "index_version isn't"),
            ({"index_version": "../pages-v20261024-1"}, "index_version isn't"),
            ({"index_version": "pages-v20261024"}, "index_version isn't"),
        )]
        for doc, text in cases:
            with self.subTest(pointer=doc):
                if doc is not None:
                    self.pointer(doc)
                done = self.run_script("--step", "publish", "--cosmos-endpoint", ENDPOINT)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(text, done.stderr)
                self.assertEqual(self.calls(), [])

    @NEEDS_NEW_BASH
    def test_a_failed_copy_records_nothing(self):
        done = self.run_script("--step", "publish", "--cosmos-endpoint", ENDPOINT, env={"STUB_FAIL": "publish-staged"})
        self.assertNotEqual(done.returncode, 0)
        calls = self.calls()
        self.assertEqual(len(calls), 1)
        self.assertIn("publish-staged", calls[0]["argv"])

    @NEEDS_NEW_BASH
    def test_a_failed_record_says_the_copy_is_done_and_how_to_retry(self):
        done = self.run_script("--step", "publish", "--cosmos-endpoint", ENDPOINT, env={"STUB_FAIL": "import-staged-run"})
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(len(self.calls()), 2)
        self.assertIn(VERSION + " is copied to Azure, but its run isn't recorded", done.stderr)
        self.assertIn("run --step publish again", done.stderr)

    def test_the_usage_shows_the_option_and_the_whole_header(self):
        done = self.run_script("--help")
        self.assertEqual(done.returncode, 2)
        self.assertIn("--cosmos-endpoint URL", done.stderr)
        self.assertIn("import-staged-run", done.stderr)
        self.assertTrue(done.stderr.rstrip().endswith("identity: usnm must be in the himds group."), done.stderr[-200:])


if __name__ == "__main__":
    unittest.main()
