#!/usr/bin/env python3
"""The pipeline dashboard's endpoint with a fake BOINC database and cache."""

import json
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


HTML = Path(__file__).resolve().parents[2] / "boinc/html/user"
ENDPOINT = HTML / "pipeline_summary.php"


@unittest.skipUnless(shutil.which("php"), "PHP is required")
class PipelineSummaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        user = self.root / "user"
        user.mkdir()
        inc = self.root / "inc"
        inc.mkdir()
        shutil.copyfile(ENDPOINT, user / ENDPOINT.name)
        (inc / "cache.inc").write_text(
            '<?php function get_path($params, $script) { return "../cache.json"; }')
        (inc / "boinc_db.inc").write_text("""<?php
function web_stopped() { return file_exists("../stop_web"); }
class BoincDb {
    static $instance;
    static function get() { die("<xml>Unexpected exiting connection path</xml>"); }
    static function get_aux($number) { self::$instance = new self(); }
    function do_query($query) {
        if (file_exists("../fail")) { throw new RuntimeException("private SQL failure"); }
        file_put_contents("../queries", $query . "\\n----\\n", FILE_APPEND);
        if (strpos($query, "FROM app a") !== false) {
            return new Rows([
                ["id" => "1", "name" => "usnm_ocr", "unsent" => "3", "in_progress" => "5", "overdue" => "1",
                 "stranded" => "2", "awaiting_validation" => "4", "returned_24h" => "40", "errors_24h" => "1",
                 "validated" => "79", "last_returned" => (string) (time() - 600)],
                ["id" => "2", "name" => "usnm_gold", "unsent" => null, "in_progress" => null, "overdue" => null,
                 "stranded" => null, "awaiting_validation" => null, "returned_24h" => null, "errors_24h" => null,
                 "validated" => null, "last_returned" => null],
            ]);
        }
        if (strpos($query, "assimilate_state = 1") !== false) {
            return new Rows([["appid" => "1", "n" => "6"], ["appid" => "99", "n" => "7"]]);
        }
        if (strpos($query, "hours_ago") !== false) {
            return new Rows([["appid" => "1", "hours_ago" => "0", "n" => "9"],
                             ["appid" => "1", "hours_ago" => "23", "n" => "2"],
                             ["appid" => "1", "hours_ago" => "24", "n" => "5"]]);
        }
        return new Rows([["active_1h" => "5", "active_24h" => "6"]]);
    }
}
class Rows {
    private $rows;
    function __construct($rows) { $this->rows = $rows; }
    function fetch_assoc() { return array_shift($this->rows); }
}
""")

    def run_endpoint(self):
        return subprocess.run(
            ["php", "-r", 'include "pipeline_summary.php"; fwrite(STDERR, "\\nHTTP:".http_response_code());'],
            cwd=self.root / "user", capture_output=True, text=True, check=False)

    def test_stages_hours_and_hosts(self):
        r = self.run_endpoint()
        self.assertNotIn("HTTP:503", r.stderr)
        d = json.loads(r.stdout)
        self.assertEqual(d["schema"], 1)
        self.assertEqual(d["hosts"], {"active_1h": 5, "active_24h": 6})
        ocr, gold = d["apps"]
        self.assertEqual((ocr["unsent"], ocr["in_progress"], ocr["overdue"], ocr["stranded"],
                          ocr["awaiting_validation"], ocr["awaiting_assimilation"]), (3, 5, 1, 2, 4, 6))
        self.assertEqual((ocr["returned_24h"], ocr["errors_24h"], ocr["validated"]), (40, 1, 79))
        # Oldest hour first; the newest hour last; a row outside the 24 hours is dropped.
        self.assertEqual(len(ocr["returned_by_hour"]), 24)
        self.assertEqual((ocr["returned_by_hour"][0], ocr["returned_by_hour"][23], sum(ocr["returned_by_hour"])),
                         (2, 9, 11))
        self.assertTrue(ocr["last_returned_at"].endswith("Z"))
        self.assertEqual((gold["unsent"], gold["stranded"], gold["last_returned_at"]), (0, 0, None))

    def test_stage_semantics_and_no_identities(self):
        self.run_endpoint()
        queries = (self.root / "queries").read_text()
        self.assertIn("r.server_state = 5 AND r.outcome = 1 AND r.validate_state = 0", queries)
        self.assertIn("h.rpc_time IS NULL OR h.rpc_time <", queries)
        self.assertIn("r.report_deadline <", queries)
        self.assertEqual(queries.count("MAX_EXECUTION_TIME(5000)"), 4)
        for word in ("INSERT", "UPDATE", "DELETE", "domain_name", "userid", "email"):
            self.assertNotIn(word, queries)

    def test_cached_for_two_minutes_and_failure_is_generic(self):
        self.run_endpoint()
        self.run_endpoint()
        self.assertEqual((self.root / "queries").read_text().count("FROM app a"), 1)
        (self.root / "cache.json").unlink()
        (self.root / "fail").touch()
        r = self.run_endpoint()
        self.assertIn("HTTP:503", r.stderr)
        self.assertNotIn("private", r.stdout)

    def test_page_reads_this_endpoint_and_tolerates_missing_counts(self):
        page = (HTML / "pipeline.html").read_text()
        self.assertIn('fetch("pipeline_summary.php"', page)
        self.assertIn("Number.isFinite(a[k]) ? a[k] : 0", page)
        self.assertNotIn("<script src=", page)


if __name__ == "__main__":
    unittest.main()
