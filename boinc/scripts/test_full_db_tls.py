"""Fail-closed evidence checks; actual runtime acceptance still requires Docker."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock

DIRECTORY = Path(__file__).parent / "db-tls"
sys.path.insert(0, str(DIRECTORY))
SPEC = importlib.util.spec_from_file_location("full_db_tls", DIRECTORY / "full.py")
full = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(full)


class FullTlsEvidenceTests(unittest.TestCase):
    def test_http_200_database_error_pages_are_not_success(self):
        full.validate_page("index.php", 200, b"<h1>usnewsmap OCR</h1>")
        full.validate_page("get_project_config.php", 200, b"<project_config/>")
        for error in (
            b"Database error", b"Database Error", b"Can't connect to database",
            b"can't CONNECT TO DATABASE", b"Cannot connect to database",
            b"Unable to connect", b"Fatal error", b"Uncaught mysqli_sql_exception",
        ):
            for path in ("index.php", "server_status.php"):
                with self.assertRaises(AssertionError):
                    full.validate_page(path, 200, b"<html>" + error + b"</html>")
        for status, body in ((200, b""), (500, b"server error")):
            with self.assertRaises(AssertionError):
                full.validate_page("index.php", status, body)

    def test_fixture_tags_are_unique_per_invocation_not_shared_per_commit(self):
        wrapper = (DIRECTORY.parent / "check-full-db-tls.sh").read_text()
        self.assertIn("uuid.uuid4().hex", wrapper)
        self.assertIn('tag="$name"', wrapper)
        self.assertNotIn("git rev-parse", wrapper)

    def test_roles_require_actual_specific_database_queries(self):
        for query, role in (
            ("select * from platform where deprecated=0", "feeder"),
            ("SELECT wu.id FROM workunit wu WHERE wu.transition_time < 123", "transitioner"),
            ("SELECT wu.id WHERE wu.need_validate > 0", "sample_trivial_validator"),
            ("select * from workunit where appid=1 and assimilate_state=1", "batch_collect_assimilator"),
        ):
            self.assertEqual(full.daemon_role(query), role)
        with self.assertRaises(RuntimeError):
            full.daemon_role("SHOW SESSION STATUS LIKE 'Ssl_cipher'")

    def test_daemon_negative_requires_nonzero_and_anchored_tls_error(self):
        for code, error, expected in (
            (1, "mysql_real_connect(): 2026: TLS/SSL error: certificate verification failed\n", True),
            (0, "mysql_real_connect(): 2026: TLS/SSL error: certificate verification failed\n", False),
            (1, "mysql_real_connect(): 1045: Access denied\n", False),
            (1, "Traceback filename mysql_real_connect(): 2026: TLS/SSL error: unrelated\n", False),
            (1, "daemon failed to start\n", False),
        ):
            result = subprocess.CompletedProcess([], code, "", error)
            self.assertEqual(full.daemon_tls_rejection(result), expected)

    def test_live_connections_need_four_different_daemon_roles_and_ciphers(self):
        rows = "1\tTLS_AES_256_GCM_SHA384\n2\tTLS_AES_256_GCM_SHA384\n3\tTLS_AES_256_GCM_SHA384\n4\tTLS_AES_256_GCM_SHA384"
        queries = ["from platform where deprecated=0", "wu.transition_time < 123",
                   "wu.need_validate > 0", "assimilate_state=1"]
        with mock.patch.object(full.fixture, "sql", side_effect=[rows, *queries, "8"]), \
                mock.patch.object(full, "emit") as emit:
            full.connections()
            self.assertEqual(emit.call_count, 5)
        for invalid_rows, invalid_queries in (
            (rows.replace("TLS_AES_256_GCM_SHA384", "", 1), queries),
            (rows, [queries[0], queries[0], *queries[2:]]),
            ("1\tTLS_AES_256_GCM_SHA384", queries),
        ):
            with mock.patch.object(full.fixture, "sql", side_effect=[invalid_rows, *invalid_queries, "8"]), \
                    mock.patch.object(full, "emit"), self.assertRaises(AssertionError):
                full.connections()

    def test_scheduler_must_return_expected_xml_and_no_dispatched_work(self):
        for body, case, succeeds in (
            (b"<scheduler_reply/>", None, True),
            (b"<scheduler_reply><project_is_down/><message>Database unavailable</message></scheduler_reply>",
             "wrong-ca", True),
            (b"<scheduler_reply><project_is_down/></scheduler_reply>", None, False),
            (b"<scheduler_reply><project_is_down/><message>Maintenance</message></scheduler_reply>",
             "wrong-ca", False),
            (b"<scheduler_reply/>", "wrong-ca", False),
            (b"<scheduler_reply><workunit/></scheduler_reply>", None, False),
            (b"<scheduler_reply><result/></scheduler_reply>", None, False),
            (b"<other/>", None, False),
        ):
            response = mock.MagicMock()
            response.__enter__.return_value.read.return_value = body
            with mock.patch.object(full.urllib.request, "urlopen", return_value=response), \
                    mock.patch.object(full, "emit"):
                if succeeds:
                    full.scheduler_rpc(case)
                else:
                    with self.assertRaises(AssertionError):
                        full.scheduler_rpc(case)

if __name__ == "__main__":
    unittest.main()
