import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class EdgeWorkerDeploy(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        scripts = self.root / "scripts/ci"
        scripts.mkdir(parents=True)
        shutil.copy2(ROOT / "scripts/ci/deploy-edge-worker.sh", scripts)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "calls"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        CALLS=str(self.log))
        self.stub(self.bin / "az", """#!/bin/sh
case "$*" in
  "keyvault secret show"*) [ "${AZ_FAIL:-0}" = 0 ] || exit 1; echo account ;;
  "containerapp show"*) echo standby.example.com ;;
  *) exit 1 ;;
esac
""")
        self.stub(self.bin / "curl", """#!/bin/sh
[ "${CURL_FAIL:-0}" = 0 ] || exit 22
while [ "$#" -gt 0 ]; do
  case "$1" in
    -D) printf 'x-usnm-origin: %s\\r\\n' "${ORIGIN:-onprem}" > "$2"; shift 2 ;;
    -o) printf ready > "$2"; shift 2 ;;
    *) shift ;;
  esac
done
""")
        self.stub(self.root / "edge/fallback-worker/deploy.sh", """#!/bin/sh
printf '%s\\n' "$@" > "$CALLS"
""")

    def stub(self, path, text):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o755)

    def run_deploy(self, **env):
        return subprocess.run(["bash", "scripts/ci/deploy-edge-worker.sh"],
                              cwd=self.root, env=dict(self.env, **env),
                              capture_output=True, text=True)

    def test_deploys_health_domain_and_heartbeat_with_scoped_ci_token(self):
        result = self.run_deploy()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = self.log.read_text().splitlines()
        for flag in ("--production", "--health-failover", "--health-heartbeat",
                     "--native-metrics", "--worker-logs"):
            self.assertIn(flag, args)
        for flag, value in (("--health-hostname", "usnewsmap.com"),
                            ("--healthcheck-hostname", "healthcheck.usnewsmap.com"),
                            ("--token-secret", "usnm-prod-workers-ci-token"),
                            ("--azure-origin", "https://standby.example.com")):
            self.assertEqual(args[args.index(flag) + 1], value)

    def test_credential_failure_stops_before_deploy(self):
        result = self.run_deploy(AZ_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log.exists())

    def test_health_errors_are_not_reported_as_success(self):
        for env in ({"CURL_FAIL": "1"}, {"ORIGIN": "unavailable"}):
            with self.subTest(env=env):
                result = self.run_deploy(**env)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("verified production", result.stdout)


if __name__ == "__main__":
    unittest.main()
