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
import tarfile
import tempfile
import time
import unittest
import importlib.util
import io
from unittest import mock
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
  "stack sub show"*) echo '{"OCR_EXCHANGE_ACCOUNT":{"value":"exchangeprod"}}' ;;
  "storage blob exists"*) echo true ;;
  "storage blob lease acquire"* | "storage blob lease release"*) ;;
  "stack group show"*)
    if [ -n "${STUB_NO_JOB:-}" ]; then echo '{}'
    else echo '{"bridgeJobName": {"type": "String", "value": "usnm-boinc-bridge-caj-prod"}}'; fi ;;
  "containerapp job show"*) printf '%s\n' "$STUB_TEMPLATE" ;;
  "containerapp job update"*) ;;
  "containerapp job start"*) echo usnm-boinc-bridge-caj-prod-abc12 ;;
  "containerapp job execution list"*)
    if [ -n "${STUB_RUNNING_JOB:-}" ]; then
      if [[ "$*" == *"--name $STUB_RUNNING_JOB "* ]]; then printf '%s' "${STUB_RUNNING:-}"; fi
    else
      printf '%s' "${STUB_RUNNING:-}"
    fi ;;
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
  esac
}
python3() {
  [[ "$*" == *"'bad-exec-'"* ]] && echo '{"tables":[{"rows":[["t1","Error: boom"]]}]}' || echo '{"tables":[{"rows":[]}]}'
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


class AcrBuildVerification(unittest.TestCase):
    """Queue/poll/digest regression tests: no Azure calls or paid builds."""

    DIGEST = "sha256:" + "a" * 64
    STUB = r"""#!/usr/bin/env python3
import json, os, pathlib, sys, time
args = sys.argv[1:]
log = pathlib.Path(os.environ["STUB_LOG"])
with log.open("a") as out:
    out.write(json.dumps(args) + "\n")
if args[:2] == ["acr", "build"]:
    if os.environ.get("STUB_STDERR"):
        print(os.environ["STUB_STDERR"], file=sys.stderr)
    print(os.environ["STUB_RUN_ID"])
    sys.exit(int(os.environ.get("STUB_SUBMIT_EXIT", "0")))
if args[:3] == ["acr", "task", "show-run"]:
    time.sleep(float(os.environ.get("STUB_RUN_DELAY", "0")))
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    count = sum(call[:3] == ["acr", "task", "show-run"] for call in calls)
    runs = json.loads(os.environ["STUB_RUNS"])
    run = runs[min(count - 1, len(runs) - 1)]
    if run == "error":
        sys.exit(3)
    print(run if isinstance(run, str) else json.dumps(run))
    sys.exit(0)
if args[:3] == ["acr", "repository", "show"]:
    time.sleep(float(os.environ.get("STUB_DIGEST_DELAY", "0")))
    print(os.environ["STUB_DIGEST"])
    sys.exit(int(os.environ.get("STUB_DIGEST_EXIT", "0")))
sys.exit("unexpected az command: " + repr(args))
"""

    def succeeded(self, **changes):
        return {"status": "Succeeded", "outputImages": [
            {"repository": "nested/image", "tag": "test", "digest": self.DIGEST, **changes}]}

    def invoke(self, runs=None, **env):
        with tempfile.TemporaryDirectory(prefix=".acr-test-", dir=ROOT) as work:
            stub = Path(work, "az")
            stub.write_text(self.STUB)
            stub.chmod(0o700)
            log = Path(work, "calls.jsonl")
            values = {**os.environ, "STUB_AZ": str(stub), "STUB_LOG": str(log),
                      "STUB_RUN_ID": "ch2c", "STUB_RUNS": json.dumps(runs or [self.succeeded()]),
                      "STUB_DIGEST": self.DIGEST, "ACR_WAIT_SECONDS": "5", "ACR_POLL_INTERVAL": "0",
                      **env}
            command = r"""
set -euo pipefail
. scripts/lib/acr-build.sh
scoped_az() { "$STUB_AZ" "$@" --subscription test-sub; }
acr_build_verified scoped_az test-registry nested/image:test 7200 \
  --platform linux/amd64 --build-arg 'BASE_IMAGE=some image' --file 'a context/Containerfile' 'a context'
echo verified
"""
            result = subprocess.run(["bash", "-c", command], cwd=ROOT, env=values,
                                    capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def assert_failed(self, runs=None, **env):
        result, calls = self.invoke(runs, **env)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("verified", result.stdout)
        self.assertEqual(sum(call[:2] == ["acr", "build"] for call in calls), 1)
        return result, calls

    def test_nonzero_cli_exit_does_not_override_successful_remote_run(self):
        result, calls = self.invoke(STUB_SUBMIT_EXIT="17")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Succeeded; verified", result.stderr)
        self.assertEqual(result.stdout.strip(), "verified")
        self.assertEqual(len(calls), 3)
        self.assertIn("--no-wait", calls[0])
        self.assertNotIn("--no-logs", calls[0])
        self.assertEqual(calls[0][-2:], ["--subscription", "test-sub"])
        self.assertIn("BASE_IMAGE=some image", calls[0])
        self.assertIn("a context/Containerfile", calls[0])
        self.assertEqual(calls[0][-3], "a context")
        self.assertEqual(calls[0][calls[0].index("--timeout") + 1], "7200")
        for call in calls:
            self.assertEqual(call[-2:], ["--subscription", "test-sub"])
        self.assertEqual(calls[1][calls[1].index("--run-id") + 1], "ch2c")

    def test_waits_through_queued_started_running_and_transient_read_failures(self):
        runs = [{"status": status} for status in ("Queued", "Started", "Running")]
        result, calls = self.invoke([*runs, "error", "not json", self.succeeded()])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(call[:2] == ["acr", "build"] for call in calls), 1)
        self.assertEqual(sum(call[:3] == ["acr", "task", "show-run"] for call in calls), 6)

    def test_failed_terminal_run_rejects_even_an_existing_valid_tag(self):
        for status in ("Failed", "Canceled", "Cancelled", "Error", "Timeout", "Unknown"):
            with self.subTest(status=status):
                result, calls = self.assert_failed([{"status": status}])
                self.assertIn(status, result.stderr)
                self.assertFalse(any(call[:2] == ["acr", "repository"] for call in calls))

    def test_submission_without_valid_run_id_never_falls_back_to_old_tag(self):
        for run_id in ("", "null", "two runs", "ch2c\nch2d", "../ch2c"):
            for exit_code in ("0", "1"):
                with self.subTest(run_id=run_id, exit_code=exit_code):
                    result, calls = self.invoke(STUB_RUN_ID=run_id, STUB_SUBMIT_EXIT=exit_code,
                                                STUB_RUNS=json.dumps([{"status": "Failed"}]))
                    self.assertNotEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(len(calls), 1)

    def test_run_id_acknowledged_only_on_stderr_is_polled(self):
        # Azure CLI 2.90: --no-wait --query runId prints nothing; stderr names the queued run.
        result, calls = self.invoke(STUB_RUN_ID="", STUB_STDERR="WARNING: Queued a build with ID: ch80")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Queued a build with ID: ch80", result.stderr)
        self.assertEqual(calls[1][calls[1].index("--run-id") + 1], "ch80")
        result, calls = self.invoke(STUB_RUN_ID="ch80", STUB_STDERR="WARNING: Queued a build with ID: ch80")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_disagreeing_or_malformed_acknowledgements_are_rejected(self):
        for run_id, stderr in (("ch2c", "WARNING: Queued a build with ID: ch80"),
                               ("", "WARNING: Queued a build with ID: ../ch80"),
                               ("", "WARNING: Queued a build with ID: ch80 and ch81"),
                               ("", "Queued a build with ID: ch80\nQueued a build with ID: ch81")):
            with self.subTest(run_id=run_id, stderr=stderr):
                result, calls = self.invoke(STUB_RUN_ID=run_id, STUB_STDERR=stderr)
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(calls), 1)

    def test_polling_timeout_never_reads_an_old_tag(self):
        for run in ({"status": "Running"}, "error", "invalid json", {}, {"status": None}):
            with self.subTest(run=run):
                result, calls = self.assert_failed([run], ACR_WAIT_SECONDS="0")
                self.assertIn("timed out", result.stderr)
                self.assertEqual(len(calls), 1)

    def test_slow_successful_and_failed_status_requests_are_bounded(self):
        for run in (self.succeeded(), {"status": "Failed"}, "error"):
            with self.subTest(run=run):
                start = time.monotonic()
                result, calls = self.assert_failed([run], ACR_WAIT_SECONDS="1", STUB_RUN_DELAY="30")
                self.assertLess(time.monotonic() - start, 4)
                self.assertIn("timed out", result.stderr)
                self.assertEqual(len(calls), 2)

    def test_slow_digest_verification_is_bounded(self):
        start = time.monotonic()
        self.assert_failed(ACR_WAIT_SECONDS="1", STUB_DIGEST_DELAY="30")
        self.assertLess(time.monotonic() - start, 4)

    def test_success_requires_valid_matching_run_output_and_registry_digest(self):
        invalid_runs = [
            {"status": "Succeeded"},
            self.succeeded(digest="sha256:bad"),
            self.succeeded(repository="other/image"),
            self.succeeded(tag="other"),
            {"status": "Succeeded", "outputImages": []},
            {"status": "Succeeded", "outputImages": self.succeeded()["outputImages"] * 2},
        ]
        for run in invalid_runs:
            with self.subTest(run=run):
                self.assert_failed([run])
        for env in ({"STUB_DIGEST": ""}, {"STUB_DIGEST": "sha256:" + "b" * 64},
                    {"STUB_DIGEST_EXIT": "1"}):
            with self.subTest(env=env):
                self.assert_failed(**env)

    def test_all_build_wrappers_use_shared_helper(self):
        expected = {"build-boinc-images.sh": 4, "boinc-app-release.sh": 3,
                    "build-quickwit.sh": 1, "stand-up.sh": 1}
        for name, count in expected.items():
            text = Path(HERE, name).read_text()
            self.assertIn(". scripts/lib/acr-build.sh", text)
            self.assertEqual(text.count("acr_build_verified "), count, name)
            # --only-show-errors would hide the queued run id (Azure CLI 2.90).
            self.assertNotIn("acr_build_verified baz ", text, name)
        # Prevent a new shell/workflow caller from reintroducing direct streaming builds/runs.
        for path in [*Path(ROOT).rglob("*.sh"), *Path(ROOT, ".github/workflows").glob("*.yml")]:
            if path.name == "acr-build.sh":
                continue
            commands = "\n".join(line for line in path.read_text().splitlines()
                                 if not line.lstrip().startswith("#"))
            self.assertNotRegex(commands, r"\b(?:az|baz)\s+acr\s+(?:build|run)\b", str(path))


class LogsFrontendRetry(unittest.TestCase):
    def setUp(self):
        patch = mock.patch.dict(os.environ, {"LOG_ANALYTICS_QUERY_HOST": "westus2.api.loganalytics.io"})
        patch.start()
        self.addCleanup(patch.stop)

    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("logs_client", Path(HERE, "log_analytics.py"))
        cls.client = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.client)

    def test_retry_classification(self):
        error = {"error": {"code": "InsufficientAccessError"}}
        for status in (403, 429, 500, 502, 503):
            self.assertTrue(self.client.retryable(status, error))
        for status, body in [(401, error), (400, error), (403, {}),
                             (403, {"error": {"code": "AuthorizationFailedError"}}),
                             (403, {"error": "InsufficientAccessError"}), (200, error)]:
            self.assertFalse(self.client.retryable(status, body))

    @staticmethod
    def response(status, body):
        response = mock.MagicMock()
        response.status = status
        response.headers = {"via": "frontend", "x-ms-request-id": "request"}
        response.read.return_value = json.dumps(body).encode()
        response.__enter__.return_value = response
        return response

    def run_query(self, responses):
        opener = mock.Mock()
        opener.open.side_effect = responses
        with mock.patch.object(self.client.urllib.request, "build_opener", return_value=opener) as fresh, \
                mock.patch.object(self.client.time, "sleep") as sleep, \
                mock.patch.object(self.client.random, "uniform", return_value=3), \
                mock.patch("sys.stderr", new_callable=io.StringIO) as stderr:
            body, history = self.client.query(SUB, "print 1", token="not-a-real-token")
        return body, history, fresh.call_count, sleep.call_count, stderr.getvalue()

    def test_fresh_connection_then_success(self):
        success = {"tables": [{"columns": [], "rows": []}]}
        result = self.run_query([
            self.response(403, {"error": {"code": "InsufficientAccessError"}}),
            self.response(200, success)])
        self.assertEqual(result[:4], (success, [
            {"attempt": 1, "status": 403, "via": "frontend", "requestId": "request",
             "host": "westus2.api.loganalytics.io"},
            {"attempt": 2, "status": 200, "via": "frontend", "requestId": "request",
             "host": "westus.api.loganalytics.io"}], 2, 0))
        self.assertNotIn("not-a-real-token", result[4])

    def test_partial_response_is_not_success(self):
        with self.assertRaises(self.client.QueryError):
            self.run_query([self.response(200, {"tables": [], "error": {"code": "PartialError"}})])

    def test_real_http_error_is_classified(self):
        error = self.client.urllib.error.HTTPError(
            "https://api.loganalytics.io/query", 403, "Forbidden",
            {"via": "frontend"}, io.BytesIO(b'{"error":{"code":"InsufficientAccessError"}}'))
        result = self.run_query([error, self.response(
            200, {"tables": [{"columns": [], "rows": []}]})])
        self.assertEqual(result[2:4], (2, 0))

    def test_transport_error_preserves_attempts_without_secret(self):
        error = self.client.urllib.error.URLError("sensitive transport payload")
        opener = mock.Mock()
        opener.open.side_effect = [self.response(
            403, {"error": {"code": "InsufficientAccessError"}})] + [error] * 11
        with mock.patch.object(self.client.urllib.request, "build_opener", return_value=opener), \
                mock.patch.object(self.client.time, "sleep"), \
                mock.patch("sys.stderr", new_callable=io.StringIO), \
                self.assertRaises(self.client.QueryError) as caught:
            self.client.query(SUB, "print 1", token="fake")
        self.assertEqual(len(caught.exception.history), 12)
        self.assertEqual(caught.exception.history[-1]["status"], "transport-error")
        self.assertNotIn("sensitive", str(caught.exception))

    def test_response_read_failure_preserves_request_metadata(self):
        response = self.response(200, {})
        response.read.side_effect = self.client.http.client.IncompleteRead(b"sensitive")
        with mock.patch.object(self.client.urllib.request, "build_opener") as opener, \
                mock.patch("sys.stderr", new_callable=io.StringIO), \
                self.assertRaises(self.client.QueryError) as caught:
            opener.return_value.open.return_value = response
            self.client.query(SUB, "print 1", token="fake")
        self.assertEqual(caught.exception.history[0]["requestId"], "request")
        self.assertEqual(caught.exception.history[0]["error"], "response-read-failed")
        self.assertNotIn("sensitive", str(caught.exception))

    def test_absolute_deadline_interrupts_continuously_slow_read(self):
        response = self.response(200, {})
        def slow_read():
            while True:
                time.sleep(0.01)
        response.read.side_effect = slow_read
        started = time.monotonic()
        with mock.patch.object(self.client.urllib.request, "build_opener") as opener, \
                mock.patch("sys.stderr", new_callable=io.StringIO), \
                self.assertRaises(self.client.QueryError) as caught:
            opener.return_value.open.return_value = response
            self.client.query(SUB, "print 1", token="fake", budget=0.1)
        self.assertLess(time.monotonic() - started, 2)
        self.assertEqual(caught.exception.history[0]["error"], "deadline-expired")

    def test_retry_cap(self):
        with self.assertRaisesRegex(self.client.QueryError, "12 attempts"):
            self.run_query([self.response(403, {"error": {"code": "InsufficientAccessError"}})] * 12)

    def test_absolute_deadline_is_not_retried_as_connect_timeout(self):
        dumps = self.client.json.dumps
        def delayed_dump(*args, **kwargs):
            time.sleep(0.08)
            return dumps(*args, **kwargs)
        def slow_connect(*args, **kwargs):
            time.sleep(0.2)
            return self.response(200, {"tables": [{"columns": [], "rows": []}]})
        started = time.monotonic()
        with mock.patch.object(self.client.json, "dumps", side_effect=delayed_dump), \
                mock.patch.object(self.client.urllib.request, "build_opener") as opener, \
                self.assertRaisesRegex(self.client.QueryError, "absolute deadline"):
            opener.return_value.open.side_effect = slow_connect
            self.client.query(SUB, "print 1", token="fake", budget=0.12)
        self.assertLess(time.monotonic() - started, 0.25)
        self.assertEqual(opener.return_value.open.call_count, 1)

    def test_socket_timeout_still_fails_over(self):
        result = self.run_query([TimeoutError("socket inactivity"),
                                 self.response(200, {"tables": [{"columns": [], "rows": []}]})])
        self.assertEqual(result[1][1]["host"], "westus.api.loganalytics.io")

    def test_nonretryable_http_error_stops_once(self):
        for status in (400, 401, 403):
            with self.subTest(status=status), self.assertRaisesRegex(self.client.QueryError, "non-retryable"):
                self.run_query([self.response(status, {"error": {"code": "OtherError"}})])

    def test_time_budget_stops_before_backoff(self):
        with mock.patch.object(self.client.time, "monotonic", side_effect=[0] + [0] * 6 + [89]), \
                mock.patch.object(self.client.time, "sleep") as sleep, \
                mock.patch.object(self.client.random, "uniform", return_value=3), \
                mock.patch.object(self.client.urllib.request, "build_opener") as opener:
            opener.return_value.open.return_value = self.response(
                403, {"error": {"code": "InsufficientAccessError"}})
            with self.assertRaises(self.client.QueryError):
                self.client.query(SUB, "print 1", token="fake")
            sleep.assert_not_called()

    def test_host_derivation_from_workspace_metadata(self):
        with mock.patch.dict(os.environ, {}, clear=True), \
                mock.patch.object(self.client.subprocess, "run") as run:
            run.return_value = mock.Mock(returncode=0, stdout='["westus2"]')
            self.assertEqual(self.client.query_host(SUB.upper(), subscription=SUB),
                             "westus2.api.loganalytics.io")
            self.assertIn(f"[?customerId=='{SUB}'].location", run.call_args.args[0])
            self.assertIn("--subscription", run.call_args.args[0])

    def test_explicit_host_and_location_and_arc_without_az(self):
        with mock.patch.dict(os.environ, {}, clear=True), \
                mock.patch.object(self.client.subprocess, "run") as run:
            self.assertEqual(self.client.query_host(SUB, location="westus2", identity="arc"),
                             "westus2.api.loganalytics.io")
            self.assertEqual(self.client.query_host(SUB, host="eastus.api.loganalytics.io"),
                             "eastus.api.loganalytics.io")
            for host in ("evil.example", "https://westus2.api.loganalytics.io", "api.loganalytics.io/"):
                with self.assertRaises(self.client.QueryError):
                    self.client.query_host(SUB, host=host)
            with self.assertRaises(self.client.QueryError):
                self.client.query_host(SUB, identity="arc")
            run.assert_not_called()

    def test_ordered_hosts_then_global_and_one_extra_pass(self):
        failure = self.response(403, {"error": {"code": "InsufficientAccessError"}})
        result = self.run_query([failure] * 6 + [
            self.response(200, {"tables": [{"columns": [], "rows": []}]})])
        self.assertEqual([x["host"] for x in result[1]],
                         [f"{x}.api.loganalytics.io" for x in
                          ("westus2", "westus", "eastus", "northcentralus", "westcentralus")]
                         + ["api.loganalytics.io", "westus2.api.loganalytics.io"])
        self.assertEqual(result[2:4], (7, 1))

    def test_regional_dns_failure_falls_back_immediately(self):
        error = self.client.urllib.error.URLError("connect failed")
        result = self.run_query([error, self.response(
            200, {"tables": [{"columns": [], "rows": []}]})])
        self.assertEqual([x["host"] for x in result[1]],
                         ["westus2.api.loganalytics.io", "westus.api.loganalytics.io"])
        self.assertEqual(result[3], 0)

    def test_configured_host_order_deduplicated_and_validated(self):
        hosts = "eastus.api.loganalytics.io, westus.api.loganalytics.io,eastus.api.loganalytics.io"
        with mock.patch.dict(os.environ, {"LOG_ANALYTICS_QUERY_HOSTS": hosts}):
            self.assertEqual(self.client.query_hosts(SUB),
                             ["eastus.api.loganalytics.io", "westus.api.loganalytics.io"])
            self.assertEqual(self.client.query_hosts(SUB, hosts="api.loganalytics.io"),
                             ["api.loganalytics.io"])
        for hosts in ("", "westus.api.loganalytics.io,", "evil.example"):
            with self.assertRaises(self.client.QueryError):
                self.client.query_hosts(SUB, hosts=hosts)

    def test_all_retryable_statuses_move_to_next_host(self):
        for status in (403, 429, 500, 503):
            with self.subTest(status=status):
                result = self.run_query([
                    self.response(status, {"error": {"code": "InsufficientAccessError"}}),
                    self.response(200, {"tables": [{"columns": [], "rows": []}]})])
                self.assertEqual([x["host"] for x in result[1]],
                                 ["westus2.api.loganalytics.io", "westus.api.loganalytics.io"])
                self.assertEqual(result[3], 0)


class WorkbookValidation(unittest.TestCase):
    def load_validator(self):
        client_spec = importlib.util.spec_from_file_location(
            "log_analytics", Path(HERE, "log_analytics.py"))
        client = importlib.util.module_from_spec(client_spec)
        client_spec.loader.exec_module(client)
        spec = importlib.util.spec_from_file_location(
            "workbook_validation", Path(HERE, "validate-boinc-workbook.py"))
        module = importlib.util.module_from_spec(spec)
        with mock.patch.dict("sys.modules", {"log_analytics": client}):
            spec.loader.exec_module(module)
        return module

    def test_exact_nine_queries_are_extracted_and_raw_messages_excluded(self):
        validator = self.load_validator()
        result = {"tables": [{"columns": [{"name": "Log_s"}, {"name": "Count"}],
                              "rows": [["private log contents", 7]]}]}
        positive = {"tables": [{"columns": [{"name": "QueryRouteControl"}], "rows": [[1]]}]}
        with mock.patch("sys.argv", ["validator", "--workspace", SUB, "--location", "westus2"]), \
                mock.patch.object(validator.log_analytics, "access_token", return_value="fake"), \
                mock.patch.object(validator.log_analytics, "query",
                                  side_effect=[(positive, [])] + [(result, [])] * 9) as query, \
                mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
                mock.patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(validator.main(), 0)
        receipt = json.loads(out.getvalue())
        self.assertEqual(len(receipt["panels"]), 10)
        self.assertEqual(query.call_count, 10)
        self.assertNotIn("private log contents", out.getvalue())
        self.assertEqual(receipt["panels"][1]["tables"][0]["rowCount"], 1)
        source = json.loads(Path(ROOT, "infra/workbooks/boinc.json").read_text())
        expected = [x["content"]["query"].replace("{TimeRange:grain}", "1h")
                    for x in source["items"] if x["type"] == 3]
        self.assertEqual([call.args[1] for call in query.call_args_list[1:]], expected)

    def test_failure_is_not_zero_rows(self):
        validator = self.load_validator()
        with mock.patch("sys.argv", ["validator", "--workspace", SUB, "--location", "westus2"]), \
                mock.patch.object(validator.log_analytics, "access_token", return_value="fake"), \
                mock.patch.object(validator.log_analytics, "query",
                                  side_effect=validator.log_analytics.QueryError("denied")), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as out, \
                mock.patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(validator.main(), 1)
        receipt = json.loads(out.getvalue())
        self.assertTrue(all(row["status"] == "failed" and "tables" not in row
                            for row in receipt["panels"]))


class LogsCallerSubscriptions(unittest.TestCase):
    def test_environment_shell_callers_forward_subscription(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("scripts/lib", "scripts/ja-ocr", "ops/queries", "bin"):
                (root / name).mkdir(parents=True)
            (root / "settings.env").write_text("fixture\n")
            (root / "ops/queries/fixture.kql").write_text("print 1")
            (root / "scripts/lib/env.sh").write_text(
                'ENV_FILE=settings.env\n'
                'aget() { case "$1" in AZURE_SUBSCRIPTION_ID) echo selected-subscription ;; '
                '*) echo workspace-guid ;; esac; }\n')
            stub = root / "bin/python3"
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CALLER_LOG"\n'
                            'echo \'{"tables":[{"columns":[],"rows":[]}]}\'\n')
            stub.chmod(0o755)
            for script, args in (
                ("logs.sh", ["--env", "prod", "--query-name", "fixture"]),
                ("ja-ocr/quality-rows.sh", ["--env", "prod", "--execution",
                                          "usnm-jaocr-caj-prod-fixture"])):
                target = root / "scripts" / script
                shutil.copy(Path(HERE, script), target)
                log = root / "arguments"
                result = subprocess.run(
                    ["bash", str(target), *args], cwd=root, text=True, capture_output=True,
                    env={**os.environ, "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"],
                         "CALLER_LOG": str(log), "AZURE_SUBSCRIPTION_ID": "wrong-default"})
                self.assertEqual(result.returncode, 0, result.stderr)
                values = log.read_text().splitlines()
                self.assertEqual(values[values.index("--subscription") + 1], "selected-subscription")

    def test_cluster_report_forwards_selected_subscription(self):
        spec = importlib.util.spec_from_file_location("cluster_report", Path(HERE, "qwcluster-report.py"))
        report = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(report)
        with mock.patch.object(report.subprocess, "run") as run:
            run.return_value = mock.Mock(stdout='{"tables":[{"columns":[],"rows":[]}]}', stderr="")
            report.log_query(SUB, "print 1", "P1D", "selected-subscription")
        args = run.call_args.args[0]
        self.assertEqual(args[args.index("--subscription") + 1], "selected-subscription")


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
                    "USNM_BOINC_LOCK_DIR": self.lock_dir,
                    # The guard's settling window after a failed start: none in tests.
                    "BOINC_START_RECONCILE_SECS": "0", "BOINC_START_RECONCILE_POLL": "0"}

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
        self.assertEqual(order, ["show", "execution", "update", "start", "execution", "execution"])

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

    def test_running_quality_or_bridge_job_blocks_only_matching_job_update(self):
        r = self.run_script("--step", "register-apps",
                            STUB_RUNNING_JOB="usnm-boinc-quality-caj-prod",
                            STUB_RUNNING="usnm-boinc-quality-caj-prod-q1\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        base = {"AZURE_CLIENT_ID": "client-2", "HANDOFF_URL": "https://x.blob.core.windows.net/boinc-handoff",
                "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/", "CURATED_URL": "https://d/curated",
                "REFERENCE_URL": "https://d/reference", "OUT_URL": "https://x/ocr-out"}
        tmpl = json.dumps({"containers": [{"name": "handoff", "env": [{"name": k, "value": v} for k, v in base.items()]}]})
        settings_home = os.path.join(self.tmp, "settings-conflict")
        env_dir = os.path.join(settings_home, "prod")
        os.makedirs(env_dir)
        with open(os.path.join(env_dir, ".env"), "w") as f:
            f.write(f'AZURE_SUBSCRIPTION_ID="{SUB}"\n')
            f.write('AZURE_RESOURCE_GROUP="rg-usnm-prod"\n')
            f.write('BOINC_HANDOFF_JOB="usnm-boinc-handoff-caj-prod"\n')
        r_handoff = self.run_script(
            "--target", "handoff", "--step", "collect", "--var", "JOB_ID=" + "a" * 64,
            STUB_TEMPLATE=tmpl, STUB_SUB=SUB, USNM_SETTINGS_HOME=settings_home,
            STUB_RUNNING_JOB="usnm-boinc-bridge-caj-prod",
            STUB_RUNNING="usnm-boinc-bridge-caj-prod-old11\n")
        self.assertEqual(r_handoff.returncode, 0, r_handoff.stderr)
        r_bridge = self.run_script(
            "--target", "bridge", "--step", "register-apps",
            STUB_RUNNING_JOB="usnm-boinc-bridge-caj-prod",
            STUB_RUNNING="usnm-boinc-bridge-caj-prod-old11\n")
        self.assertEqual(r_bridge.returncode, 1)
        self.assertIn("still running usnm-boinc-bridge-caj-prod-old11", r_bridge.stderr)

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
        self.assertEqual(params["projectUrl"]["value"], "https://boinc.usnewsmap.com/")
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
        for pc in ("docker_mt", "docker_nvidia_cuda", "docker_intel_vulkan"):
            r = self.run_release("--app", "usnm_surya", "--plan-class", pc, "--worker-image", img)
            self.assertIn("--signing-host must be a host name", r.stderr)
        r = self.run_release("--app", "usnm_surya", "--plan-class", "intel_vulkan_8b", "--worker-image", img)
        self.assertEqual(r.returncode, 1)
        self.assertIn("unsupported --app and --plan-class", r.stderr)
        r = self.run_release("--app", "usnm_qwen_train", "--plan-class", "docker_nvidia_train", "--worker-image", img)
        self.assertIn("--signing-host must be a host name", r.stderr)

    def test_prepare_only_is_recognized_before_signing_host_validation(self):
        r = self.run_release("--app", "usnm_ocr", "--prepare-only", "--what-if")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--signing-host must be a host name", r.stderr)

    def test_native_amd_allows_only_pinned_vulkan_platforms(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as folder:
            binary = Path(folder) / "usnm_qwen"
            binary.write_bytes(b"fixture")
            for platform in ("x86_64-pc-linux-gnu", "windows_x86_64",
                             "aarch64-unknown-linux-gnu", "arm64-apple-darwin", "x86_64-apple-darwin"):
                r = subprocess.run(
                    [APP_RELEASE_SCRIPT, "--env", "prod", "--version", "1.0",
                     "--signing-host", "fixture.invalid", "--app", "usnm_qwen",
                     "--kind", "native", "--binary", str(binary),
                     "--plan-class", "amd_vulkan_8b", "--platform", platform],
                    cwd=ROOT, capture_output=True, text=True,
                )
                self.assertNotEqual(r.returncode, 0)
                if platform in ("x86_64-pc-linux-gnu", "windows_x86_64"):
                    self.assertIn("requires --extra-file llama-server", r.stderr)
                else:
                    self.assertIn("native --plan-class amd_vulkan_8b is not supported", r.stderr)
            r = self.run_release("--app", "usnm_surya", "--kind", "native",
                                 "--binary", str(binary), "--plan-class", "amd_vulkan_8b")
            self.assertIn("--app and --plan-class for --kind native", r.stderr)

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
    def test_native_extra_archive_contains_only_requested_files(self):
        text = Path(APP_RELEASE_SCRIPT).read_text()
        command = next(line.strip().split(" | ssh_host", 1)[0]
                       for line in text.splitlines() if 'tar -C "$work" -c extra |' in line)
        with tempfile.TemporaryDirectory() as tmp:
            extra = Path(tmp, "extra")
            extra.mkdir()
            library = extra / "libggml.so"
            library.write_bytes(b"native-library")
            if os.uname().sysname == "Darwin":
                subprocess.run(["xattr", "-w", "com.usnm.release-test", "metadata", str(library)],
                               check=True, capture_output=True)
            archive = subprocess.run(["bash", "-c", command],
                                     env=dict(os.environ, work=tmp),
                                     capture_output=True, check=True)
            with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as bundle:
                self.assertEqual(sorted(name.rstrip("/") for name in bundle.getnames()),
                                 ["extra", "extra/libggml.so"])

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
            CLAIM_LANGUAGES="eng,fra",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 3, calls)
        self.assertTrue(calls[0].startswith("claim pull --queue-url https://usnmst.queue.core.windows.net "))
        self.assertIn("--manifest-version pages-v1", calls[0])
        self.assertIn("--max 7 --lease-hours 96", calls[0])
        self.assertIn("--nonce abcdefgh12 --queue ocr-bulk --queue ocr-priority "
                      "--language eng --language fra", calls[0])
        # The pop receipts exist only in the claim files: they go up first.
        self.assertIn("handoff put", calls[1])
        self.assertIn("--prefix claims", calls[1])
        self.assertIn("--prefix scheduler", calls[2])

    def test_handoff_claim_refuses_bad_settings(self):
        base = dict(QUEUE_URL="https://usnmst.queue.core.windows.net",
                    HANDOFF_MANIFEST="pages-v1/manifest.parquet", CLAIM_NONCE="abcdefgh12")
        for bad in (dict(QUEUE_URL="http://x"), dict(CLAIM_MAX="101"), dict(CLAIM_MAX="0"),
                    dict(CLAIM_QUEUES="ocr-deadletter"), dict(CLAIM_NONCE="short"),
                    dict(CLAIM_LEASE_HOURS="169"), dict(HANDOFF_MANIFEST="../manifest.parquet"),
                    dict(CLAIM_LANGUAGES="en"), dict(CLAIM_LANGUAGES="eng,ENG"),
                    dict(CLAIM_LANGUAGES="eng;x")):
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

    def test_handoff_collect_batch_goes_on_past_a_failed_job(self):
        # #932: JOB_IDS collects a tick's results in one execution; each runs the single-job path.
        j1, j2, j3 = "1" * 64, "2" * 64, "3" * 64
        self.stub('case "$*" in\n'
                  '  "handoff get-spool"*) for a in "$@"; do [ "$prev" = --trust-out ] && echo "{}" > "$a"; prev=$a; done; echo 8 ;;\n'
                  f'  "collect"*"--job {j2}"*) exit 4 ;;\n'
                  'esac')
        r = self.handoff("collect", JOB_IDS=f"{j1},{j2},{j3}")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("collect failed for 1 of 3 jobs", r.stderr)
        collects = [c for c in self.calls() if c.startswith("collect ")]
        self.assertEqual([c.split("--job ")[1].split()[0] for c in collects], [j1, j2, j3])
        self.assertTrue(all("--host-id 8" in c for c in collects))

    def test_handoff_collect_batch_rejects_mixed_or_malformed_ids(self):
        j1 = "1" * 64
        for extra in ({"JOB_IDS": j1, "JOB_ID": j1}, {"JOB_IDS": "not-a-job"},
                      {"JOB_IDS": ",".join([j1] * 101)}):
            with self.subTest(extra=extra):
                r = self.handoff("collect", **extra)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertFalse([c for c in self.calls() if c.startswith("collect ")])

    def bridge_copy(self):
        share = os.path.join(self.tmp, "share")
        proj = os.path.join(share, "usnmocr")
        os.makedirs(proj, exist_ok=True)
        open(os.path.join(proj, ".usnm-initialised"), "w").close()
        bridge = os.path.join(self.tmp, "bridge-job.sh")
        lib_path = os.path.join(ROOT, "contrib-compute/images/server/lib.sh")
        with open(BRIDGE_JOB_SCRIPT) as src, open(bridge, "w") as dst:
            dst.write(src.read().replace("/usr/local/lib/usnm-boinc/lib.sh", lib_path))
        os.chmod(bridge, 0o755)
        env = {**self.env, "KEY_VAULT_NAME": "usnmboinckvprod", "DB_HOST": "db.example.org",
               "DB_NAME": "usnmocr", "MASTER_HOST": "boinc.usnewsmap.com", "PROJECT_DIR": share,
               "HANDOFF_DIR": os.path.join(share, "handoff"),
               "HANDOFF_URL": "https://usnmst.blob.core.windows.net/boinc-handoff"}
        return bridge, env

    def bridge_batch_copy(self, fail_job):
        """bridge-job.sh's header and batch modes, with the single-job path after them replaced by
        a stub that logs each run and fails for `fail_job` (the real path needs Key Vault)."""
        bridge, env = self.bridge_copy()
        text = open(bridge).read()
        marker = "# End of the batch modes (#932).\n"
        self.assertIn(marker, text)
        stub = ('printf "single %s %s %s\\n" "$step" "${JOB_ID:-}" "${HANDOFF_SPOOL:-}" >> "$STUB_LOG"\n'
                f'[ "${{JOB_ID:-}}" != {fail_job} ] || exit 4\nexit 0\n')
        # Key Vault and the config: stubs that log, so a test can count what a batch fetches (#958).
        lib_line = f". {os.path.join(ROOT, 'contrib-compute/images/server/lib.sh')}\n"
        secrets = ('SECRETS="$STUB_SECRETS"\n'
                   'fetch_secrets() { echo "stub fetch_secrets" >> "$STUB_LOG"; mkdir -p "$SECRETS/keys"; '
                   'echo x > "$SECRETS/db-runtime"; echo x > "$SECRETS/keys/upload_private"; '
                   'echo x > "$SECRETS/keys/upload_public"; }\n'
                   'render_config() { echo "stub render_config" >> "$STUB_LOG"; }\n'
                   'kv_get() { echo "stub kv_get $1" >> "$STUB_LOG"; mkdir -p "$(dirname "$2")"; echo x > "$2"; }\n')
        head = text.split(marker)[0]
        self.assertIn(lib_line, head)
        with open(bridge, "w") as f:
            f.write(head.replace(lib_line, lib_line + secrets, 1) + marker + stub)
        env = {**env, "STUB_SECRETS": os.path.join(self.tmp, "secrets")}
        return bridge, env

    def test_bridge_submit_remote_batch_stops_at_the_first_failure(self):
        # #932: JOB_IDS submits a tick's jobs in one execution, stopping at the first failure
        # (the next tick reconciles that job before submitting anything else).
        j1, j2, j3 = "1" * 64, "2" * 64, "3" * 64
        bridge, env = self.bridge_batch_copy(j2)
        r = subprocess.run([bridge], cwd=ROOT, env={**env, "BRIDGE_STEP": "submit-remote",
                                                    "JOB_IDS": f"{j1},{j2},{j3}"},
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn(f"submit-remote stopped at {j2}", r.stderr)
        self.assertEqual([c.split()[1:3] for c in self.calls() if c.startswith("single ")],
                         [["submit-remote", j1], ["submit-remote", j2]])
        # One fetch of every secret and one config for the whole batch; no per-job Key Vault read.
        stubs = [c for c in self.calls() if c.startswith("stub ")]
        self.assertEqual(stubs, ["stub fetch_secrets", "stub render_config"])

    def test_bridge_spool_out_batch_goes_on_past_a_failed_job(self):
        j1, j2, j3 = "1" * 64, "2" * 64, "3" * 64
        spools = {j: f"/project/usnmocr/results/{i}" for i, j in enumerate((j1, j2, j3))}
        bridge, env = self.bridge_batch_copy(j2)
        r = subprocess.run([bridge], cwd=ROOT, env={**env, "BRIDGE_STEP": "spool-out",
                                                    "HANDOFF_SPOOLS": ",".join(f"{j}={s}" for j, s in spools.items())},
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("spool-out failed for 1 of 3 jobs", r.stderr)
        self.assertEqual([c.split()[1:4] for c in self.calls() if c.startswith("single ")],
                         [["spool-out", j, spools[j]] for j in (j1, j2, j3)])
        # Spool-out needs only the database password, read once for the batch.
        stubs = [c for c in self.calls() if c.startswith("stub ")]
        self.assertEqual(stubs, ["stub kv_get mysql-runtime-password"])

    def test_bridge_batches_reject_mixed_or_malformed_input(self):
        j1 = "1" * 64
        bridge, env = self.bridge_copy()
        for extra in ({"BRIDGE_STEP": "submit-remote", "JOB_IDS": j1, "JOB_ID": j1},
                      {"BRIDGE_STEP": "submit-remote", "JOB_IDS": "x"},
                      {"BRIDGE_STEP": "spool-out", "HANDOFF_SPOOLS": f"{j1}=/etc/passwd/..", "JOB_ID": j1},
                      {"BRIDGE_STEP": "spool-out", "HANDOFF_SPOOLS": "nope"}):
            with self.subTest(extra=extra):
                r = subprocess.run([bridge], cwd=ROOT, env={**env, **extra}, capture_output=True, text=True)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertFalse([c for c in self.calls() if c.startswith(("submit ", "handoff put-spool"))])

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


class CrossHostStartGuard(StartBridge):
    # Reuse launcher fixtures; the remote lease is an atomic directory in the
    # private test tree, shared by processes with different host-local locks.
    def setUp(self):
        super().setUp()
        self.remote = Path(self.tmp, "remote")
        self.remote.mkdir()
        self.env["STUB_REMOTE_STATE"] = str(self.remote)
        az = Path(self.tmp, "bin", "az")
        az.write_text(r"""#!/usr/bin/env bash
case "$*" in
  "storage blob exists"*)
    if [ -d "$STUB_REMOTE_STATE/blob" ]; then echo true; else echo false; fi
    exit ;;
  "storage blob upload"*)
    printf '%s\n' "$*" >> "$STUB_LOG"
    mkdir "$STUB_REMOTE_STATE/blob" || exit 9
    [ -z "${STUB_UPLOAD_RACE:-}" ]; exit ;;
  "storage blob lease acquire"*)
    printf '%s\n' "$*" >> "$STUB_LOG"
    mkdir "$STUB_REMOTE_STATE/held" 2>/dev/null; exit ;;
  "storage blob lease release"*)
    printf '%s\n' "$*" >> "$STUB_LOG"
    [ -z "${STUB_RELEASE_FAILURE:-}" ] || exit 9
    rmdir "$STUB_REMOTE_STATE/held"; exit ;;
  "stack sub show"*)
    [ -z "${STUB_NO_EXCHANGE:-}" ] || { echo '{}'; exit; } ;;
  "containerapp job update"*)
    [ -z "${STUB_UPDATE_FAILURE:-}" ] || exit 9 ;;
  "containerapp job show"*"provisioningState"*)
    echo "${STUB_PROVISIONING:-Succeeded}"; exit ;;
  "containerapp job execution list"*"[].name"*)
    # The guard's reconciliation listing: counted, and scripted from a given call on.
    printf '%s\n' "$*" >> "$STUB_LOG"
    n=$(( $(cat "$STUB_REMOTE_STATE/lists" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$STUB_REMOTE_STATE/lists"
    if [ -n "${STUB_LIST_FAIL_FROM:-}" ] && [ "$n" -ge "$STUB_LIST_FAIL_FROM" ]; then exit 9; fi
    if [ -n "${STUB_LIST_VISIBLE_FROM:-}" ] && [ "$n" -ge "$STUB_LIST_VISIBLE_FROM" ]; then echo usnm-boinc-bridge-caj-prod-abc12; fi
    exit ;;
  "containerapp job execution list"*)
    if [ -n "${STUB_ACTIVE_VISIBLE:-}" ] && [ -f "$STUB_REMOTE_STATE/started" ]; then
      printf '%s\n' "$*" >> "$STUB_LOG"
      echo usnm-boinc-bridge-caj-prod-abc12
      exit
    fi ;;
  "containerapp job start"*)
    touch "$STUB_REMOTE_STATE/started"
    [ -z "${STUB_START_FAILURE:-}" ] || exit 9
    if [ -n "${STUB_PAUSE_START:-}" ]; then
      touch "$STUB_REMOTE_STATE/starting"
      while [ ! -f "$STUB_REMOTE_STATE/continue" ]; do sleep 0.02; done
    fi ;;
  "containerapp job execution show"*"--query properties.status"*)
    [ -z "${STUB_STATUS_FAILURE:-}" ] || exit 9 ;;
esac
""" + STUB_AZ.partition("\n")[2])

    def test_cross_host_lease_denies_second_starter_before_template_write(self):
        first = subprocess.Popen(
            [SCRIPT, "--env", "prod", "--step", "register-apps"], cwd=ROOT,
            env={**self.env, "STUB_PAUSE_START": "1"},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 10
            while not (self.remote / "starting").exists() and time.monotonic() < deadline:
                self.assertIsNone(first.poll(), "first launcher exited before start barrier")
                time.sleep(0.02)
            self.assertTrue((self.remote / "starting").exists())
            second = self.run_script("--step", "submit",
                                     USNM_BOINC_LOCK_DIR=str(Path(self.tmp, "other-host.lock")))
            self.assertEqual(second.returncode, 1, second.stderr)
            self.assertIn("cross-host start guard", second.stderr)
            self.assertEqual(sum(c.startswith("containerapp job update") for c in self.calls()), 1)
            (self.remote / "continue").touch()
            out, err = first.communicate(timeout=10)
            self.assertEqual(first.returncode, 0, err)
            self.assertIn("abc12", out)
            self.assertFalse((self.remote / "held").exists())
            self.assertFalse(Path(self.lock_dir).exists())
        finally:
            if first.poll() is None:
                first.terminate()
                first.communicate(timeout=10)

    def test_busy_remote_lease_refuses_handoff_too(self):
        (self.remote / "blob").mkdir()
        (self.remote / "held").mkdir()
        settings = Path(self.tmp, "settings", "prod")
        settings.mkdir(parents=True)
        (settings / ".env").write_text(
            f'AZURE_SUBSCRIPTION_ID="{SUB}"\nAZURE_RESOURCE_GROUP="rg-prod"\n'
            'BOINC_HANDOFF_JOB="handoff-prod"\n')
        values = dict(AZURE_CLIENT_ID="client", HANDOFF_URL="https://exchangeprod.blob.core.windows.net/boinc-handoff",
                      COSMOS_ENDPOINT="https://cosmos", CURATED_URL="https://curated",
                      REFERENCE_URL="https://reference", OUT_URL="https://out")
        tmpl = json.dumps({"containers": [{"name": "handoff", "env": [
            {"name": k, "value": v} for k, v in values.items()]}]})
        result = self.run_script("--target", "handoff", "--step", "collect",
                                 STUB_TEMPLATE=tmpl, USNM_SETTINGS_HOME=str(settings.parent))
        self.assertEqual(result.returncode, 1)
        self.assertIn("cross-host start guard", result.stderr)
        self.assertFalse(any("job update" in c or "job start" in c for c in self.calls()))

    def test_missing_storage_discovery_fails_closed(self):
        result = self.run_script("--step", "register-apps", STUB_NO_EXCHANGE="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no OCR_EXCHANGE_ACCOUNT", result.stderr)
        self.assertFalse(any("job update" in c or "job start" in c for c in self.calls()))
        self.assertFalse(Path(self.lock_dir).exists())

    def test_unknown_nonterminal_status_is_checked_before_update(self):
        result = self.run_script("--step", "register-apps", STUB_RUNNING="pending-execution")
        self.assertEqual(result.returncode, 1)
        self.assertIn("still running pending-execution", result.stderr)
        [query] = [c for c in self.calls() if c.startswith("containerapp job execution list")]
        self.assertIn("properties.status!='Succeeded'", query)
        self.assertNotIn("status=='Running'", query)
        self.assertFalse((self.remote / "held").exists())

    def test_ambiguous_update_start_and_failed_readback_release_once_listed(self):
        # A failed update, start or read-back: the job's executions are listed for the settling
        # window; when every listing succeeds, the lease is released (the next launcher sees
        # anything that did start) and the run still fails.
        # The update and start failures leave no execution name; the status and settings
        # failures have one, released once the list shows it.
        for failure in ({"STUB_UPDATE_FAILURE": "1"}, {"STUB_START_FAILURE": "1"},
                        {"STUB_STATUS_FAILURE": "1", "STUB_LIST_VISIBLE_FROM": "1"},
                        {"STUB_EXEC_ENV": '[{"name":"BRIDGE_STEP","value":"wrong"}]', "STUB_LIST_VISIBLE_FROM": "1"}):
            with self.subTest(failure=failure):
                for f in ("started", "lists"):
                    (self.remote / f).unlink(missing_ok=True)
                result = self.run_script("--step", "register-apps", **failure)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("releasing the cross-host lease", result.stderr)
                self.assertNotIn("lease retained", result.stderr)
                self.assertFalse((self.remote / "held").exists())
                self.assertTrue(any(c.startswith("containerapp job execution list") and "[].name" in c
                                    for c in self.calls()))
                self.assertFalse(Path(self.lock_dir).exists())
                self.assertFalse(list(Path(ROOT).glob(".boinc-start-*")))

    def ticking_clock(self):
        # Each `date` call is 10 s later, so the settling window is walked deterministically.
        clock = Path(self.tmp, "bin", "date")
        clock.write_text('#!/usr/bin/env bash\n'
                         'n=$(( $(cat "$STUB_REMOTE_STATE/now" 2>/dev/null || echo 1000) + 10 ))\n'
                         'echo "$n" > "$STUB_REMOTE_STATE/now"; echo "$n"\n')
        clock.chmod(0o755)
        return {"BOINC_START_RECONCILE_SECS": "30", "BOINC_START_RECONCILE_POLL": "0"}

    def reconcile_lists(self):
        f = self.remote / "lists"
        return int(f.read_text()) if f.exists() else 0

    def test_unknown_start_holds_the_lease_for_the_whole_settling_window(self):
        result = self.run_script("--step", "register-apps", STUB_START_FAILURE="1", **self.ticking_clock())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("releasing the cross-host lease", result.stderr)
        # 30 s at 10 s a tick: listed until the deadline, not released on the first clean list.
        self.assertGreaterEqual(self.reconcile_lists(), 3)
        self.assertFalse((self.remote / "held").exists())

    def test_known_execution_is_released_when_a_later_poll_shows_it(self):
        result = self.run_script("--step", "register-apps", STUB_STATUS_FAILURE="1",
                                 STUB_LIST_VISIBLE_FROM="2", **self.ticking_clock())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("releasing the cross-host lease", result.stderr)
        self.assertEqual(self.reconcile_lists(), 2)
        self.assertFalse((self.remote / "held").exists())

    def test_a_later_listing_failure_retains_the_lease(self):
        result = self.run_script("--step", "register-apps", STUB_START_FAILURE="1",
                                 STUB_LIST_FAIL_FROM="2", **self.ticking_clock())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue((self.remote / "held").exists())

    def test_template_update_still_in_flight_retains_the_lease(self):
        result = self.run_script("--step", "register-apps", STUB_UPDATE_FAILURE="1",
                                 STUB_PROVISIONING="InProgress", **self.ticking_clock())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue((self.remote / "held").exists())

    def test_known_execution_missing_from_the_list_retains_remote_lease(self):
        result = self.run_script("--step", "register-apps", STUB_STATUS_FAILURE="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue((self.remote / "held").exists())

    def test_ambiguous_start_retains_remote_lease_when_listing_fails(self):
        result = self.run_script("--step", "register-apps", STUB_START_FAILURE="1",
                                 STUB_LIST_FAIL_FROM="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue((self.remote / "held").exists())
        self.assertFalse(Path(self.lock_dir).exists())

    def test_simultaneous_marker_creation_is_safe(self):
        result = self.run_script("--step", "register-apps", STUB_UPLOAD_RACE="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        [upload] = [c for c in self.calls() if c.startswith("storage blob upload")]
        self.assertIn("--overwrite false", upload)
        self.assertFalse((self.remote / "held").exists())

    def test_unobservable_execution_times_out_without_unlocking(self):
        clock = Path(self.tmp, "bin", "date")
        clock.write_text('#!/usr/bin/env bash\n'
                         'if [ -f "$STUB_REMOTE_STATE/clock" ]; then echo 121; '
                         'else touch "$STUB_REMOTE_STATE/clock"; echo 0; fi\n')
        clock.chmod(0o755)
        result = self.run_script("--step", "register-apps", STUB_EXEC_STATUS="Running")
        self.assertEqual(result.returncode, 1)
        self.assertIn("start visibility is uncertain", result.stderr)
        self.assertIn("lease retained", result.stderr)
        self.assertTrue((self.remote / "held").exists())
        self.assertFalse(Path(self.lock_dir).exists())

    def test_active_execution_is_visible_before_unlocking_without_wait(self):
        result = self.run_script("--step", "register-apps", STUB_EXEC_STATUS="Running",
                                 STUB_ACTIVE_VISIBLE="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        lists = [i for i, c in enumerate(calls) if c.startswith("containerapp job execution list")]
        release = next(i for i, c in enumerate(calls) if c.startswith("storage blob lease release"))
        self.assertEqual(len(lists), 2)
        self.assertLess(lists[-1], release)
        self.assertFalse((self.remote / "held").exists())

    def test_wait_mins_releases_remote_lease_before_status_poll(self):
        result = self.run_script("--step", "register-apps", "--wait-mins", "5")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        release = next(i for i, c in enumerate(calls) if c.startswith("storage blob lease release"))
        status_polls = [i for i, c in enumerate(calls) if "--query properties.status" in c]
        self.assertEqual(len(status_polls), 2)
        self.assertLess(status_polls[0], release)
        self.assertLess(release, status_polls[1])
        self.assertEqual(sum(c.startswith("storage blob lease release") for c in calls), 1)
        self.assertFalse((self.remote / "held").exists())

    def test_release_failure_is_reported_and_local_mutex_is_cleaned(self):
        result = self.run_script("--step", "register-apps", STUB_RELEASE_FAILURE="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("lease release failed", result.stderr)
        self.assertTrue((self.remote / "held").exists())
        self.assertFalse(Path(self.lock_dir).exists())

    def test_controller_lease_is_never_recursively_acquired(self):
        result = self.run_script("--step", "register-apps")
        self.assertEqual(result.returncode, 0, result.stderr)
        lease_calls = [c for c in self.calls() if c.startswith("storage blob lease")]
        self.assertEqual(len(lease_calls), 2)
        self.assertTrue(all("scheduler/starts/" in c for c in lease_calls))
        self.assertFalse(any("scheduler/controller.lock" in c for c in lease_calls))
        self.assertIn("--lease-duration -1", lease_calls[0])
        self.assertFalse(list(Path(ROOT).glob(".boinc-start-*")))

if __name__ == "__main__":
    unittest.main()
