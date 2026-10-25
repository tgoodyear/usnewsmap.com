"""Tests for scripts/onprem/release.sh's publish, restore and ready steps (#1061):
python3 -m unittest -v scripts/onprem/test_release.py. Needs bash, curl and jq. Nothing
here reaches Azure: usnm-ingest is a stub that records its arguments, and the app the
ready step polls is a server on the loopback interface."""

import http.server
import json
import os
import subprocess
import tempfile
import threading
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
RELEASE = os.path.join(HERE, "release.sh")
VERSION = "pages-v20261024-1"
OTHER = "pages-v20261023-1"

STUB_INGEST = """#!/bin/sh
for arg in "$@"; do printf '%s\\n' "$arg"; done > "$STUB_ARGS"
"""


class ReleaseScript(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = self.tmp.name
        self.run_dir = os.path.join(self.dir, "run")
        self.args_file = os.path.join(self.dir, "ingest-args")
        self.ingest = os.path.join(self.dir, "usnm-ingest")
        with open(self.ingest, "w") as f:
            f.write(STUB_INGEST)
        os.chmod(self.ingest, 0o755)

    def release(self, *args, **env):
        environ = dict(os.environ)
        environ.update(
            USNM_INGEST=self.ingest,
            USNM_LAKE="lakeexample",
            USNM_INDEX_ACCOUNT="accountexample",
            STUB_ARGS=self.args_file,
            USNM_READY_WAIT_SECS="0",
            NO_PROXY="127.0.0.1,localhost",
            no_proxy="127.0.0.1,localhost",
        )
        environ.update(env)
        return subprocess.run(
            ["bash", RELEASE, "--run-dir", self.run_dir, *args],
            capture_output=True,
            text=True,
            env=environ,
            timeout=60,
        )

    def ingest_args(self):
        with open(self.args_file) as f:
            return f.read().split("\n")[:-1]

    def stage(self):
        reference = os.path.join(self.run_dir, "reference")
        os.makedirs(reference)
        with open(os.path.join(reference, "index-storage-pending.json"), "w") as f:
            json.dump({"index_version": VERSION}, f)


class PublishAndRestore(ReleaseScript):
    def test_publish_passes_no_restore(self):
        self.stage()
        for extra in ([], ["--dry-run"]):
            p = self.release("--step", "publish", *extra)
            self.assertEqual(p.returncode, 0, p.stderr)
            args = self.ingest_args()
            self.assertEqual(args[args.index("publish-staged") + 1 :], [
                "--local-index-root", self.run_dir + "/qw",
                "--to-reference", "https://lakeexample.blob.core.windows.net/reference",
                "--to-index", "https://accountexample.blob.core.windows.net/qw-index",
                *extra,
            ])  # fmt: skip
            self.assertNotIn("--restore", args)

    def test_restore_passes_the_flag_to_the_same_copy(self):
        self.stage()
        for extra in ([], ["--dry-run"]):
            p = self.release("--step", "restore", *extra)
            self.assertEqual(p.returncode, 0, p.stderr)
            args = self.ingest_args()
            self.assertEqual(args[args.index("publish-staged") + 1 :], [
                "--local-index-root", self.run_dir + "/qw",
                "--to-reference", "https://lakeexample.blob.core.windows.net/reference",
                "--to-index", "https://accountexample.blob.core.windows.net/qw-index",
                *extra, "--restore",
            ])  # fmt: skip
            self.assertEqual(args[:4], ["--state-file", self.run_dir + "/state.json", "--curated",
                                        "https://lakeexample.blob.core.windows.net/curated"])  # fmt: skip

    def test_restore_needs_the_run_that_built_the_version(self):
        p = self.release("--step", "restore")
        self.assertEqual(p.returncode, 1)
        self.assertIn("index-storage-pending.json: restore from the run directory", p.stderr)
        self.assertFalse(os.path.exists(self.args_file), "usnm-ingest ran")

    def test_the_usage_lists_the_new_steps_and_only_the_header(self):
        p = self.release("--help")
        self.assertEqual(p.returncode, 2)
        for wanted in (
            "--step restore [--dry-run]",
            "--step ready --app-url URL --index-version V",
            "restore  copies the version the lake already serves",
            "ready    waits until the app",
            "identity: usnm must be in the himds group.",
        ):
            self.assertIn(wanted, p.stderr)
        self.assertNotIn("set -euo pipefail", p.stderr)
        p = self.release("--step", "nothing")
        self.assertEqual(p.returncode, 1)
        self.assertIn("publish, restore or ready", p.stderr)


class App(http.server.BaseHTTPRequestHandler):
    """A stand-in API: /readyz and /v1/meta, as `served` says. With `meta_cut` set, /v1/meta
    advertises more bytes than it sends and closes the connection: a complete JSON body that
    curl still reports as a failed transfer."""

    def do_GET(self):
        served = self.server.served
        served["requests"].append(self.path)
        if self.path == "/readyz":
            failing = served["readyz_failures"] > 0
            served["readyz_failures"] -= 1 if failing else 0
            self.reply(503 if failing else 200, b"not ready" if failing else b"ready")
        elif self.path == "/v1/meta":
            body = json.dumps({"index_version": served["version"], "pages": 1}).encode()
            self.reply(200, body, advertised=len(body) + 100 if served["meta_cut"] else None)
        else:
            self.reply(404, b"")

    def reply(self, status, body, advertised=None):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body) if advertised is None else advertised))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Ready(ReleaseScript):
    def serve(self, version=VERSION, readyz_failures=0, meta_cut=False):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), App)
        server.served = {
            "version": version,
            "readyz_failures": readyz_failures,
            "meta_cut": meta_cut,
            "requests": [],
        }
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server

    def ready(self, server, *args, **env):
        url = "http://127.0.0.1:%d" % server.server_address[1]
        return self.release("--step", "ready", "--app-url", url, "--index-version", VERSION, *args, **env)

    def test_the_app_serving_the_version_is_ready(self):
        server = self.serve()
        p = self.ready(server)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("serves " + VERSION, p.stdout)
        self.assertEqual(server.served["requests"], ["/readyz", "/v1/meta"])

    def test_it_needs_no_usnm_ingest(self):
        p = self.ready(self.serve(), USNM_INGEST=os.path.join(self.dir, "missing"))
        self.assertEqual(p.returncode, 0, p.stderr)

    def test_an_app_that_becomes_ready_is_waited_for(self):
        server = self.serve(readyz_failures=2)
        p = self.ready(server, USNM_READY_TRIES="5")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("try 2 of 5: /readyz doesn't pass", p.stderr)
        self.assertEqual(server.served["requests"].count("/readyz"), 3)

    def test_another_version_fails_after_the_tries(self):
        server = self.serve(version=OTHER)
        p = self.ready(server, USNM_READY_TRIES="3")
        self.assertEqual(p.returncode, 1)
        self.assertIn("after 3 tries", p.stderr)
        self.assertIn("/v1/meta names " + OTHER, p.stderr)
        self.assertEqual(server.served["requests"].count("/v1/meta"), 3)

    def test_a_meta_response_that_fails_to_arrive_in_full_is_not_ready(self):
        server = self.serve(meta_cut=True)
        p = self.ready(server, USNM_READY_TRIES="2")
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertNotIn("ready: ", p.stdout)
        self.assertIn("after 2 tries", p.stderr)
        self.assertIn("/v1/meta doesn't answer", p.stderr)
        self.assertEqual(server.served["requests"].count("/v1/meta"), 2)

    def test_an_app_that_never_gets_ready_fails_without_asking_its_version(self):
        server = self.serve(readyz_failures=100)
        p = self.ready(server, USNM_READY_TRIES="2")
        self.assertEqual(p.returncode, 1)
        self.assertIn("/readyz doesn't pass", p.stderr)
        self.assertNotIn("/v1/meta", server.served["requests"])

    def test_an_app_that_does_not_answer_fails(self):
        server = self.serve()
        url = "http://127.0.0.1:%d" % server.server_address[1]
        server.shutdown()
        server.server_close()
        p = self.release("--step", "ready", "--app-url", url, "--index-version", VERSION,
                         USNM_READY_TRIES="2")  # fmt: skip
        self.assertEqual(p.returncode, 1)
        self.assertIn("after 2 tries", p.stderr)

    def test_it_needs_the_url_and_the_version_and_takes_only_web_urls(self):
        p = self.release("--step", "ready", "--index-version", VERSION)
        self.assertEqual((p.returncode, "needs --app-url" in p.stderr), (1, True), p.stderr)
        p = self.release("--step", "ready", "--app-url", "https://app.example")
        self.assertEqual((p.returncode, "needs --index-version" in p.stderr), (1, True), p.stderr)
        for url in ("ftp://app.example", "http://app.example", "https://app.example/path",
                    "https://app.example?x=1", "-o/tmp/x"):  # fmt: skip
            p = self.release("--step", "ready", "--app-url", url, "--index-version", VERSION)
            self.assertEqual(p.returncode, 1, url)
            self.assertIn("--app-url: https://HOST", p.stderr)
        p = self.release("--step", "ready", "--app-url", "https://app.example",
                         "--index-version", VERSION, USNM_READY_TRIES="0")  # fmt: skip
        self.assertIn("USNM_READY_TRIES", p.stderr)


if __name__ == "__main__":
    unittest.main()
