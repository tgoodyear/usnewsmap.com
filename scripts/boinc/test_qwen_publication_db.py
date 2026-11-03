"""Actual MySQL concurrency at the fresh-pair policy publication boundary.

USNM_QWEN_MYSQL_PORT opts into a disposable localhost database (root, no password),
as used by the CI MySQL service. No production database or scheduler is contacted.
"""
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/boinc/native-qwen-cohort.php"
CONNECT = r"""
mysqli_report(MYSQLI_REPORT_ERROR | MYSQLI_REPORT_STRICT);
$db = new mysqli("127.0.0.1", "root", "", getenv("FIXTURE_DATABASE"),
                 intval(getenv("USNM_QWEN_MYSQL_PORT")));
"""
DATABASE_BRIDGE = r"""<?php
class BoincDb {
    private static $instance;
    public $db_conn;
    private $counts = 0;
    function __construct() {
        mysqli_report(MYSQLI_REPORT_ERROR | MYSQLI_REPORT_STRICT);
        $this->db_conn = new mysqli("127.0.0.1", "root", "", getenv("FIXTURE_DATABASE"),
                                  intval(getenv("USNM_QWEN_MYSQL_PORT")));
    }
    static function get() { return self::$instance ??= new self(); }
    function do_query($sql) {
        return $this->db_conn->query(str_replace("DBNAME", getenv("FIXTURE_DATABASE"), $sql));
    }
    function count($table, $where) {
        $count = intval($this->do_query("SELECT COUNT(*) FROM $table WHERE $where")->fetch_row()[0]);
        if (++$this->counts === intval(getenv("FIXTURE_PAUSE_AT"))) {
            fwrite(STDOUT, "validated\n");
            fflush(STDOUT);
            if (fgets(STDIN) === false) throw new RuntimeException("fixture barrier closed");
        }
        return $count;
    }
}
class BoincWorkunit {
    static function lookup_id($id) {
        return BoincDb::get()->do_query("SELECT * FROM workunit WHERE id=$id")->fetch_object();
    }
}
class BoincApp {
    static function lookup_id($id) {
        return BoincDb::get()->do_query("SELECT * FROM app WHERE id=$id")->fetch_object();
    }
}
class BoincResult {
    static function enum($where) {
        $rows = BoincDb::get()->do_query("SELECT * FROM result WHERE $where");
        $out = [];
        while ($row = $rows->fetch_object()) $out[] = $row;
        return $out;
    }
}
"""


@unittest.skipUnless(os.environ.get("USNM_QWEN_MYSQL_PORT"), "disposable MySQL not configured")
class PublicationDatabase(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(shutil.which("php"), "PHP with mysqli is required for the live fixture")
        self.directory = tempfile.TemporaryDirectory(prefix=".qwen-publication-db-", dir=ROOT)
        self.addCleanup(self.directory.cleanup)
        self.project = Path(self.directory.name)
        self.database = "usnm_qwen_race_" + uuid.uuid4().hex
        self.env = os.environ | {"FIXTURE_DATABASE": self.database}
        self.sql("CREATE DATABASE `" + self.database + "`", database="")
        self.addCleanup(self.sql, "DROP DATABASE `" + self.database + "`", database="")
        inc = self.project / "html/inc"
        inc.mkdir(parents=True)
        (inc / "boinc_db.inc").write_text(DATABASE_BRIDGE)
        (self.project / "config.xml").write_text(
            "<boinc><config><one_result_per_host_per_wu/></config></boinc>")
        self.original = (ROOT / "contrib-compute/boinc/plan_class_spec.xml").read_bytes()
        self.policy = self.project / "plan_class_spec.xml"
        self.children = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=10)

    def sql(self, query, database=None):
        env = self.env if database is None else self.env | {"FIXTURE_DATABASE": database}
        result = subprocess.run(
            ["php", "-r", CONNECT + """
$result = $db->query(stream_get_contents(STDIN));
echo json_encode($result === true ? [] : $result->fetch_all(MYSQLI_ASSOC));
"""], input=query, env=env, text=True, capture_output=True, timeout=15, check=True)
        return json.loads(result.stdout)

    def reset(self, engine):
        self.policy.write_bytes(self.original)
        self.sql("DROP TABLE IF EXISTS app, workunit, result, assignment")
        columns = {
            "app": "id INT PRIMARY KEY, name VARCHAR(32)",
            "workunit": (
                "id INT PRIMARY KEY, appid INT, batch INT, rsc_disk_bound BIGINT, min_quorum INT, "
                "target_nresults INT, canonical_resultid INT, error_mask INT, transitioner_flags INT, "
                "need_validate INT, app_version_num INT, hr_class INT"),
            "result": (
                "id INT PRIMARY KEY, workunitid INT, server_state INT, sent_time INT, hostid INT, "
                "outcome INT, validate_state INT, app_version_id INT, received_time INT, "
                "INDEX(workunitid)"),
            "assignment": "id INT AUTO_INCREMENT PRIMARY KEY, workunitid INT, INDEX(workunitid)",
        }
        for table, fields in columns.items():
            self.sql(f"CREATE TABLE {table} ({fields}) ENGINE={engine}")
        self.sql("INSERT INTO app VALUES (6, 'usnm_qwen')")
        self.sql("INSERT INTO workunit VALUES (100,6,8,19327352832,2,2,0,0,0,0,0,0)")
        self.sql("INSERT INTO result VALUES (200,100,2,0,0,0,0,0,0),(201,100,2,0,0,0,0,0,0)")

    def start(self, command, **env):
        child = subprocess.Popen(command, env=self.env | env, text=True,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        return child

    def line(self, child):
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            self.assertTrue(selector.select(timeout=10), "fixture process did not reach its barrier")
        line = child.stdout.readline().strip()
        self.assertTrue(line, child.stderr.read() if child.poll() is not None else "empty fixture output")
        return line

    def publisher(self, pause_at=2):
        return self.start([
            "php", str(HELPER), "--project", str(self.project), "--plan-class", "shared_8b",
            "--workunit", "100", "--fresh-pair", "--admit-until", str(int(time.time()) + 60), "--apply",
        ], FIXTURE_PAUSE_AT=str(pause_at))

    def writer(self, query):
        return self.start(["php", "-r", CONNECT + r"""
echo $db->thread_id, "\n";
fflush(STDOUT);
$db->query($argv[1]);
echo json_encode(["policy_sha256" => hash_file("sha256", $argv[2]),
                  "affected" => $db->affected_rows]), "\n";
""", query, str(self.policy)])

    def assert_blocked(self, child, thread):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and child.poll() is None:
            rows = self.sql(f"SELECT STATE FROM information_schema.processlist WHERE ID={int(thread)}")
            if rows and "lock" in rows[0]["STATE"].lower():
                return
            time.sleep(0.02)
        self.fail("database writer was not blocked by the publication guard")

    def test_assignment_and_workunit_writes_wait_until_policy_publication(self):
        for engine in ("InnoDB", "MyISAM"):
            for query in (
                "UPDATE result SET server_state=4, hostid=9, sent_time=1 WHERE id=200",
                "INSERT INTO assignment (workunitid) VALUES (100)",
                "UPDATE workunit SET error_mask=1 WHERE id=100",
            ):
                with self.subTest(engine=engine, query=query):
                    self.reset(engine)
                    publisher = self.publisher()
                    self.assertEqual(self.line(publisher), "validated")
                    writer = self.writer(query)
                    self.assert_blocked(writer, self.line(writer))
                    output, error = publisher.communicate("\n", timeout=10)
                    self.assertEqual(publisher.returncode, 0, error)
                    proof = json.loads(output)
                    written, error = writer.communicate(timeout=10)
                    self.assertEqual(writer.returncode, 0, error)
                    observed = json.loads(written)
                    self.assertEqual(observed["affected"], 1)
                    self.assertEqual(observed["policy_sha256"], proof["after_sha256"])
                    self.assertNotEqual(proof["before_sha256"], proof["after_sha256"])
                    self.assertEqual(proof["fresh_pair_guard"], "mysql-read-lock-v1")

    def test_failure_after_validation_preserves_policy_and_releases_writer(self):
        self.reset("InnoDB")
        (self.project / "ops-issue-559").write_text("publication must fail")
        publisher = self.publisher()
        self.assertEqual(self.line(publisher), "validated")
        writer = self.writer("UPDATE result SET server_state=4, hostid=9, sent_time=1 WHERE id=200")
        self.assert_blocked(writer, self.line(writer))
        _, error = publisher.communicate("\n", timeout=10)
        self.assertNotEqual(publisher.returncode, 0)
        self.assertIn("Cannot create journal", error)
        output, error = writer.communicate(timeout=10)
        self.assertEqual(writer.returncode, 0, error)
        self.assertEqual(json.loads(output)["policy_sha256"], hashlib.sha256(self.original).hexdigest())
        self.assertEqual(self.policy.read_bytes(), self.original)

    def test_writer_winning_before_guard_is_rejected_by_final_revalidation(self):
        self.reset("InnoDB")
        publisher = self.publisher(pause_at=1)
        self.assertEqual(self.line(publisher), "validated")
        self.sql("UPDATE result SET server_state=4, hostid=9, sent_time=1 WHERE id=200")
        _, error = publisher.communicate("\n", timeout=10)
        self.assertNotEqual(publisher.returncode, 0)
        self.assertIn("two unsent results", error)
        self.assertEqual(self.policy.read_bytes(), self.original)
        self.assertFalse((self.project / "ops-issue-559").exists())

    def test_killed_publisher_releases_database_without_exposing_policy(self):
        self.reset("InnoDB")
        publisher = self.publisher()
        self.assertEqual(self.line(publisher), "validated")
        writer = self.writer("UPDATE result SET server_state=4, hostid=9, sent_time=1 WHERE id=200")
        self.assert_blocked(writer, self.line(writer))
        publisher.kill()
        publisher.communicate(timeout=10)
        output, error = writer.communicate(timeout=10)
        self.assertEqual(writer.returncode, 0, error)
        self.assertEqual(json.loads(output)["policy_sha256"], hashlib.sha256(self.original).hexdigest())
        self.assertEqual(self.policy.read_bytes(), self.original)

    def test_lost_database_session_before_rename_never_exposes_policy(self):
        self.reset("InnoDB")
        publisher = self.publisher()
        self.assertEqual(self.line(publisher), "validated")
        rows = self.sql("SELECT ID FROM information_schema.processlist "
                        f"WHERE DB='{self.database}' AND COMMAND='Sleep'")
        self.assertEqual(len(rows), 1)
        self.sql("KILL CONNECTION " + str(int(rows[0]["ID"])))
        self.sql("UPDATE result SET server_state=4, hostid=9, sent_time=1 WHERE id=200")
        publisher.communicate("\n", timeout=10)
        self.assertNotEqual(publisher.returncode, 0)
        self.assertEqual(self.policy.read_bytes(), self.original)
        self.assertEqual(list(self.project.glob(".native-qwen-cohort-*")), [])

    def test_lock_wait_is_bounded_and_never_opens_policy(self):
        self.reset("InnoDB")
        publisher = self.publisher(pause_at=1)
        self.assertEqual(self.line(publisher), "validated")
        holder = self.start(["php", "-r", CONNECT + """
$db->query("LOCK TABLES result WRITE");
echo "held\\n";
fflush(STDOUT);
fgets(STDIN);
$db->query("UNLOCK TABLES");
"""])
        self.assertEqual(self.line(holder), "held")
        start = time.monotonic()
        _, error = publisher.communicate("\n", timeout=10)
        self.assertNotEqual(publisher.returncode, 0)
        self.assertIn("Lock wait timeout", error)
        self.assertLess(time.monotonic() - start, 8)
        holder.communicate("\n", timeout=10)
        self.assertEqual(self.policy.read_bytes(), self.original)


if __name__ == "__main__":
    unittest.main()
