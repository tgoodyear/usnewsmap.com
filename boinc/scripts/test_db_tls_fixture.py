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
        errors = {
            "python": '{"mysql_errno":2026}',
            "php-legacy": '{"mysql_errno":2002,"message":"Cannot connect to MySQL using SSL"}',
            "php-DbConn": '{"mysql_errno":2002,"message":"Cannot connect to MySQL using SSL"}',
            "cpp-DB_CONN": "mysql_real_connect(): 2026: TLS/SSL error: certificate verify failed\n",
            "cli": "ERROR 2026 (HY000): TLS/SSL error: certificate verify failed\n",
        }
        for consumer, error in errors.items():
            with self.subTest(consumer=consumer):
                self.assertTrue(fixture.tls_rejection("wrong-ca", consumer,
                                                     subprocess.CompletedProcess([], 10, "", error)))
        for result in (subprocess.CompletedProcess([], 0, "", ""),
                       subprocess.CompletedProcess([], 1, "", "missing PHP module"),
                       subprocess.CompletedProcess([], 1, "", "Access denied")):
            with self.assertRaises(RuntimeError):
                self.matrix(result, False)

    def test_php_authentication_traceback_cannot_be_mistaken_for_tls_evidence(self):
        traceback = """PHP Fatal error: Uncaught mysqli_sql_exception: Access denied for user 'fixture'@'tls-db.test'
Stack trace:
#0 /opt/boinc/html/inc/db.inc(30): mysqli_real_connect()
#1 /opt/boinc/html/inc/db.inc(55): usnm_mysql_tls_connect()
#2 /checks/consumer.php(7): _mysql_connect()
#3 {main}
  thrown in /opt/boinc/html/inc/db.inc on line 30
"""
        for consumer in ("php-legacy", "php-DbConn"):
            for diagnostic in (traceback, json.dumps({"mysql_errno": 1045, "message": traceback})):
                for case in ("wrong-ca", "wrong-hostname", "plaintext-peer"):
                    self.assertFalse(fixture.tls_rejection(
                        case, consumer, subprocess.CompletedProcess([], 10, "", diagnostic)))

    def test_missing_tcp_reachability_is_not_a_tls_negative_pass(self):
        with patch.object(fixture.socket, "create_connection", side_effect=ConnectionRefusedError), \
                patch.object(fixture.subprocess, "run") as run:
            with self.assertRaises(ConnectionRefusedError):
                fixture.matrix("wrong-hostname", False)
            run.assert_not_called()

    def test_swallowed_dbconn_error_is_accepted_only_for_confirmed_plaintext_case(self):
        diagnostic = '{"mysql_errno":2006,"message":"MySQL server has gone away"}'
        for consumer in ("php-legacy", "php-DbConn"):
            for case in ("plaintext-peer", "wrong-hostname", "wrong-ca"):
                with self.subTest(case=case, consumer=consumer):
                    self.assertEqual(fixture.tls_rejection(
                        case, consumer, subprocess.CompletedProcess([], 10, "", diagnostic)),
                        case == "plaintext-peer")


if __name__ == "__main__":
    unittest.main()
