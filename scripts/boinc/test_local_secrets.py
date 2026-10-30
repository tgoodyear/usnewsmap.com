"""Root-owned credential integration tests, also run on the on-prem Linux host."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


LIB = Path(__file__).resolve().parents[2] / "contrib-compute/images/server/lib.sh"


@unittest.skipUnless(os.geteuid() == 0, "local credential ownership requires root")
class LocalSecrets(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.secrets = self.root / "credentials"
        self.secrets.mkdir(mode=0o700)
        self.source = self.secrets / "mysql-runtime-password"
        self.source.write_text("synthetic-local-value\n")
        self.source.chmod(0o600)
        self.output = self.root / "runtime"
        self.env = {
            **os.environ,
            "BOINC_SECRETS_DIR": str(self.secrets),
            "DB_HOST": "example.invalid",
            "DB_NAME": "test",
            "MASTER_HOST": "example.invalid",
            "PROJECT_DIR": str(self.root),
        }

    def tearDown(self):
        self.tmp.cleanup()

    def read(self, function="kv_get", name="mysql-runtime-password"):
        command = f'. "{LIB}"; kv_token() {{ echo unexpected-cloud-request; exit 99; }}; '
        command += f'{function} "{name}" "{self.output}"'
        return subprocess.run(["bash", "-eu", "-c", command], env=self.env,
                              capture_output=True, text=True, timeout=10)

    def test_reads_local_without_key_vault_or_logging_value(self):
        done = self.read()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.output.read_text(), "synthetic-local-value\n")
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("synthetic-local-value", done.stdout + done.stderr)

    def test_missing_required_never_falls_back_to_cloud(self):
        self.source.unlink()
        done = self.read()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("cannot read local secret", done.stderr)
        self.assertNotIn("unexpected-cloud-request", done.stdout + done.stderr)

    def test_optional_absent_is_logged_but_invalid_is_fatal(self):
        done = self.read("kv_get_optional", "code-sign-public-key")
        self.assertEqual(done.returncode, 1)
        self.assertIn("in local secrets", done.stderr)
        self.source.chmod(0o644)
        done = self.read("kv_get_optional")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("cannot read local secret", done.stderr)
        self.assertNotIn("unexpected-cloud-request", done.stdout + done.stderr)

    def test_rejects_symlinks_and_insecure_parent(self):
        self.source.unlink()
        self.source.symlink_to(self.root / "elsewhere")
        done = self.read()
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse(self.output.exists())
        self.source.unlink()
        self.secrets.chmod(0o755)
        done = self.read()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("root-owned mode 0700", done.stderr)


if __name__ == "__main__":
    unittest.main()
