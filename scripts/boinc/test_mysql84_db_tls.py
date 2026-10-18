"""Unit guards for MySQL 8.4 evidence and the non-activating infrastructure gate."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

DIRECTORY = Path(__file__).parent / "db-tls"
ROOT = DIRECTORY.parents[2]
sys.path.insert(0, str(DIRECTORY))
SPEC = importlib.util.spec_from_file_location("mysql84", DIRECTORY / "mysql84.py")
mysql84 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mysql84)
sys.path.pop(0)


class MySQL84Evidence(unittest.TestCase):
    def test_observer_requires_exact_server_family_and_enforcement(self):
        for value, valid in (("8.4.7\t1\n", True), ("8.0.44\t1\n", False),
                             ("8.4.7\t0\n", False), ("10.11.19-MariaDB\t1\n", False)):
            with self.subTest(value=value), \
                    patch.object(mysql84.fixture, "run", return_value=subprocess.CompletedProcess([], 0, value)), \
                    patch.object(Path, "read_bytes", return_value=b"patch"), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                if valid:
                    mysql84.observe()
                    self.assertTrue(json.loads(output.getvalue())["require_secure_transport"])
                else:
                    with self.assertRaises(RuntimeError):
                        mysql84.observe()

    def test_plaintext_requires_policy_error_not_authentication_or_network_failure(self):
        for result, expected in ((subprocess.CompletedProcess([], 1, "", "ERROR 3159 (HY000): insecure transport prohibited\n"), True),
                                 (subprocess.CompletedProcess([], 0, "", "ERROR 3159 (HY000): insecure transport prohibited\n"), False),
                                 (subprocess.CompletedProcess([], 1, "", "ERROR 1045 (28000): Access denied\n"), False),
                                 (subprocess.CompletedProcess([], 1, "", "ERROR 2002 (HY000): connection refused\n"), False)):
            self.assertEqual(mysql84.plaintext_rejected(result), expected)

    def test_restart_requires_fresh_policy_observation_and_actual_consumer_matrix(self):
        with patch.object(sys, "argv", ["mysql84.py", "restart"]), \
                patch.object(mysql84.shutil, "copyfile"), patch.object(Path, "write_text"), \
                patch.object(mysql84, "observe") as observe, \
                patch.object(mysql84.fixture, "matrix") as matrix:
            mysql84.main()
            observe.assert_called_once_with()
            matrix.assert_called_once_with("mysql84-tls-restart", True)

    def test_invalid_policy_stops_before_connector_evidence(self):
        with patch.object(sys, "argv", ["mysql84.py", "matrix"]), \
                patch.object(mysql84.shutil, "copyfile"), patch.object(Path, "write_text"), \
                patch.object(mysql84, "observe", side_effect=RuntimeError("not enforcing")), \
                patch.object(mysql84.fixture, "matrix") as matrix:
            with self.assertRaises(RuntimeError):
                mysql84.main()
            matrix.assert_not_called()

    def test_bicep_gate_is_off_by_default_and_forwarded_without_production_activation(self):
        main = (ROOT / "contrib-compute/infra/boinc/main.bicep").read_text()
        module = (ROOT / "contrib-compute/infra/boinc/modules/mysql.bicep").read_text()
        for source in (main, module):
            self.assertIn("param requireSecureTransport bool = false", source)
        self.assertIn("requireSecureTransport: requireSecureTransport", main)
        self.assertIn("value: requireSecureTransport ? 'ON' : 'OFF'", module)

    def test_private_wrapper_uses_existing_mysql84_pin_and_persistent_restart(self):
        wrapper = (DIRECTORY.parent / "check-mysql84-db-tls.sh").read_text()
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        image = next(line.split("=", 1)[1] for line in wrapper.splitlines() if line.startswith("mysql_image="))
        self.assertIn("image: " + image, ci)
        self.assertIn("scripts/boinc/check-mysql84-db-tls.sh", ci)
        self.assertIn("--require-secure-transport=ON", wrapper)
        self.assertIn('for phase in matrix restart', wrapper)
        self.assertIn('--network none', wrapper)
        self.assertIn('--network "container:$name-db"', wrapper)
        self.assertNotIn('--publish', wrapper)
        self.assertNotIn('--privileged', wrapper)
        self.assertNotIn('docker.sock', wrapper)


if __name__ == "__main__":
    unittest.main()
