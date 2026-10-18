#!/usr/bin/env python3
"""Exercise the public PHP endpoint with a fake BOINC database and cache."""

import json
import re
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path


ENDPOINT = Path(__file__).resolve().parents[2] / "boinc/html/user/work_summary.php"


@unittest.skipUnless(shutil.which("php"), "PHP is required")
class WorkSummaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.user = root / "user"
        self.user.mkdir()
        inc = root / "inc"
        inc.mkdir()
        shutil.copyfile(ENDPOINT, self.user / ENDPOINT.name)
        (inc / "cache.inc").write_text(
            '<?php function get_path($params, $script) { return "../cache.json"; }'
        )
        (inc / "boinc_db.inc").write_text("""<?php
function web_stopped() { return file_exists("../stop_web"); }
class BoincDb {
    static $instance;
    static function get() { die("<xml>Unexpected exiting connection path</xml>"); }
    static function get_aux($number) {
        self::$instance = file_exists("../connection_failure") ? null : new self();
    }
    function do_query($query) {
        if (file_exists("../fail")) { throw new RuntimeException("private SQL failure"); }
        if (strpos($query, "server_state = 5 AND r.outcome = 1 AND r.validate_state = 1") === false) {
            throw new RuntimeException("Incorrect completion semantics");
        }
        file_put_contents("../queries", "query\\n", FILE_APPEND);
        return new SummaryRows();
    }
}
class SummaryRows {
    private $rows = [
        ["name" => "usnm_qwen", "ready" => "7", "in_progress" => "2", "validated" => "4"],
        ["name" => "usnm_gold", "ready" => null, "in_progress" => null, "validated" => null]
    ];
    function fetch_assoc() { return array_shift($this->rows); }
}
""")

    def run_endpoint(self):
        result = subprocess.run(
            ["php", "-r", 'include "work_summary.php"; fwrite(STDERR, "\\nHTTP:".http_response_code());'],
            cwd=self.user, capture_output=True, text=True, check=True,
        )
        return json.loads(result.stdout), result.stderr

    def test_counts_are_numeric_and_cached(self):
        data, _ = self.run_endpoint()
        self.assertEqual(data["schema"], 1)
        self.assertEqual(data["apps"][0], {
            "name": "usnm_qwen", "ready": 7, "in_progress": 2, "validated": 4,
        })
        self.assertEqual(data["apps"][1]["validated"], 0)
        again, _ = self.run_endpoint()
        self.assertEqual(again, data)
        self.assertEqual((self.user.parent / "queries").read_text(), "query\n")

    def test_database_errors_are_unavailable_not_zero_or_private_details(self):
        (self.user.parent / "fail").touch()
        data, errors = self.run_endpoint()
        self.assertEqual(data, {"error": "BOINC task counts are temporarily unavailable."})
        self.assertIn("HTTP:503", errors)
        self.assertNotIn("private SQL failure", errors)
        self.assertIn("BOINC work summary refresh failed", errors)

    def test_expired_cache_refreshes(self):
        (self.user.parent / "cache.json").write_text(
            '{"schema":1,"updated_at":"2000-01-01T00:00:00Z","apps":[]}'
        )
        data, _ = self.run_endpoint()
        self.assertEqual(len(data["apps"]), 2)

    def test_invalid_cache_recovers_under_the_lock(self):
        for cached in ('{"schema":1,', '{"schema":1,"apps":[]}', 'null'):
            with self.subTest(cached=cached):
                (self.user.parent / "cache.json").write_text(cached)
                data, errors = self.run_endpoint()
                self.assertEqual(len(data["apps"]), 2)
                self.assertIn("invalid cache; refreshing from database", errors)
                self.assertNotIn("HTTP:503", errors)

    def test_connection_failure_and_maintenance_return_json_errors(self):
        for marker in ("connection_failure", "stop_web"):
            with self.subTest(marker=marker):
                path = self.user.parent / marker
                path.touch()
                try:
                    data, errors = self.run_endpoint()
                    self.assertEqual(data, {"error": "BOINC task counts are temporarily unavailable."})
                    self.assertIn("HTTP:503", errors)
                    self.assertFalse((self.user.parent / "queries").exists())
                finally:
                    path.unlink()

    def test_maintenance_does_not_serve_a_fresh_cached_success(self):
        self.run_endpoint()
        (self.user.parent / "stop_web").touch()
        data, errors = self.run_endpoint()
        self.assertIn("error", data)
        self.assertIn("HTTP:503", errors)

    def test_concurrent_refresh_is_unavailable_not_a_second_query(self):
        import fcntl
        with (self.user.parent / "cache.json").open("w") as file:
            fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
            data, errors = self.run_endpoint()
        self.assertIn("error", data)
        self.assertIn("HTTP:503", errors)
        self.assertFalse((self.user.parent / "queries").exists())


class SummaryQueryTests(unittest.TestCase):
    def test_only_successfully_validated_results_count_as_completed(self):
        query = re.search(r'do_query\("(.+?)"\);', ENDPOINT.read_text(), re.S)[1]
        with closing(sqlite3.connect(":memory:")) as db:
            db.executescript("""
                CREATE TABLE app(id INTEGER, name TEXT);
                CREATE TABLE result(appid INTEGER, server_state INTEGER, outcome INTEGER, validate_state INTEGER);
                INSERT INTO app VALUES(1, 'usnm_qwen'), (2, 'usnm_gold');
                INSERT INTO result VALUES
                    (1, 2, 0, 0), (1, 4, 0, 0),
                    (1, 5, 1, 1), (1, 5, 1, 1),
                    (1, 5, 1, 0), (1, 5, 1, 2),
                    (1, 5, 3, 1), (1, 1, 0, 0);
            """)
            self.assertEqual(db.execute(query).fetchall(), [
                ("usnm_qwen", 1, 1, 2),
                ("usnm_gold", None, None, None),
            ])


if __name__ == "__main__":
    unittest.main()
