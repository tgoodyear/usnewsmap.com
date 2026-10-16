"""Fail-closed evidence checks; the actual database matrix runs separately in Docker."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("db_tls_fixture", Path(__file__).parent / "db-tls/fixture.py")
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class FixtureEvidence(unittest.TestCase):
    def matrix(self, result, succeeds):
        output = io.StringIO()
        with patch.object(fixture.socket, "create_connection"), \
                patch.object(fixture.subprocess, "run", return_value=result), \
                contextlib.redirect_stdout(output):
            fixture.matrix("test", succeeds)
        return [json.loads(line) for line in output.getvalue().splitlines()]

    def test_valid_requires_actual_nonempty_cipher_from_each_consumer(self):
        rows = self.matrix(subprocess.CompletedProcess([], 0, "TLS_AES_256_GCM_SHA384\n", ""), True)
        self.assertEqual(len(rows), 5)
        self.assertEqual(len({row["consumer"] for row in rows}), 5)
        self.assertTrue(all(row["outcome"] == "encrypted" for row in rows))
        for result in (subprocess.CompletedProcess([], 0, "", ""),
                       subprocess.CompletedProcess([], 1, "TLS_AES_256_GCM_SHA384", "")):
            with self.assertRaises(RuntimeError):
                self.matrix(result, True)

    def test_negative_requires_failure_and_tls_diagnostic_not_an_unrelated_crash(self):
        rows = self.matrix(subprocess.CompletedProcess([], 1, "", "TLS/SSL: certificate verify failed"), False)
        self.assertTrue(all(row["outcome"] == "rejected" and row["cipher"] is None for row in rows))
        for result in (subprocess.CompletedProcess([], 0, "", ""),
                       subprocess.CompletedProcess([], 1, "", "missing PHP module"),
                       subprocess.CompletedProcess([], 1, "", "Access denied")):
            with self.assertRaises(RuntimeError):
                self.matrix(result, False)

    def test_missing_tcp_reachability_is_not_a_tls_negative_pass(self):
        with patch.object(fixture.socket, "create_connection", side_effect=ConnectionRefusedError), \
                patch.object(fixture.subprocess, "run") as run:
            with self.assertRaises(ConnectionRefusedError):
                fixture.matrix("wrong-hostname", False)
            run.assert_not_called()

    def test_swallowed_dbconn_error_is_accepted_only_for_confirmed_plaintext_case(self):
        tls_error = subprocess.CompletedProcess([], 1, "", "TLS/SSL error")
        swallowed = subprocess.CompletedProcess([], 10, "", "MySQL server has gone away")
        for case in ("plaintext-peer", "wrong-hostname", "wrong-ca"):
            with self.subTest(case=case), patch.object(fixture.socket, "create_connection"), \
                    patch.object(fixture.subprocess, "run",
                                 side_effect=[tls_error, tls_error, swallowed, tls_error, tls_error]), \
                    contextlib.redirect_stdout(io.StringIO()):
                if case == "plaintext-peer":
                    fixture.matrix(case, False)
                else:
                    with self.assertRaises(RuntimeError):
                        fixture.matrix(case, False)


if __name__ == "__main__":
    unittest.main()
