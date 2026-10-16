"""Tests for the BOINC work-volume JSON export and its workbook consumers."""

import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("work_volume", HERE / "work-volume.py")
work_volume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(work_volume)

ROOT = HERE.parents[2]


class WorkVolume(unittest.TestCase):
    def test_runtime_defaults_preserve_carriage_returns(self):
        source = (HERE / "bridge-job.sh").read_text().split(
            'if [ "$step" = work-volume ] || [ "$step" = batch-inventory ]; then\n', 1)[1]
        setup = source.split('  if [ "$step" = batch-inventory ]; then', 1)[0]
        for password, escaped in (("internal\rreturn", "internal\\rreturn"),
                                  ("internal\r\nreturn", "internal\\r\\nreturn"),
                                  ("trailing\r", "trailing\\r"),
                                  ("trailing\r\n", "trailing\\r\\n")):
            with self.subTest(password=repr(password)):
                with tempfile.TemporaryDirectory(prefix="work-volume-") as directory:
                    script = '''
set -eu
kv_get() { printf '%s\\n' "$SYNTHETIC_PASSWORD" > "$2"; }
''' + setup + '\ntrap - EXIT\n'
                    subprocess.run(
                        ["bash", "-c", script], check=True, capture_output=True,
                        env={**os.environ, "SECRETS": directory, "DB_HOST": "localhost",
                             "DB_RUNTIME_USER": "synthetic", "SYNTHETIC_PASSWORD": password},
                    )
                    self.assertEqual(
                        (Path(directory) / "mysql-runtime.cnf").read_bytes(),
                        ('[client]\nhost="localhost"\nuser="synthetic"\npassword="'
                         + escaped + '"\n').encode("utf-8"),
                    )

    def test_runtime_defaults_are_valid_and_private(self):
        source = (HERE / "bridge-job.sh").read_text().split(
            'if [ "$step" = work-volume ] || [ "$step" = batch-inventory ]; then\n', 1)[1]
        setup = source.split('  if [ "$step" = batch-inventory ]; then', 1)[0]
        with tempfile.TemporaryDirectory(prefix="work-volume-") as directory:
            directory = Path(directory)
            password = ' synthetic#;"\\value\twith spaces '
            script = '''
set -eu
kv_get() { printf '%s\\n' "$SYNTHETIC_PASSWORD" > "$2"; }
''' + setup + '\ntrap - EXIT\n'
            result = subprocess.run(
                ["bash", "-c", script], check=True, capture_output=True, text=True,
                env={**os.environ, "SECRETS": str(directory), "DB_HOST": "localhost",
                     "DB_RUNTIME_USER": "synthetic", "SYNTHETIC_PASSWORD": password},
            )
            self.assertEqual(result.stdout, "")
            self.assertEqual(result.stderr, "")
            defaults = directory / "mysql-runtime.cnf"
            self.assertEqual(defaults.stat().st_mode & 0o777, 0o600)
            self.assertEqual(defaults.read_text(), '[client]\nhost="localhost"\nuser="synthetic"\n'
                             + 'password' + '=" synthetic#;\\"\\\\value\\twith spaces "\n')
            if shutil.which("mysql"):
                parsed = subprocess.run(
                    ["mysql", f"--defaults-file={defaults}", "--help"],
                    capture_output=True, text=True,
                )
                self.assertEqual(parsed.returncode, 0)
            if os.environ.get("USNM_WORK_VOLUME_MYSQL_PORT"):
                connection = ["mysql", "--protocol=TCP", "--host=127.0.0.1",
                              f"--port={os.environ['USNM_WORK_VOLUME_MYSQL_PORT']}"]
                admin = connection[:1] + ["--no-defaults"] + connection[1:] + ["--user=root"]
                quoted = password.replace("\\", "\\\\").replace("'", "''")
                subprocess.run(admin, input="CREATE USER 'synthetic'@'%' IDENTIFIED BY '"
                               + quoted + "';", text=True, capture_output=True, check=True)
                try:
                    authenticated = subprocess.run(
                        connection[:1] + [f"--defaults-file={defaults}"] + connection[1:]
                        + ["--batch", "--skip-column-names", "--execute=SELECT 1"],
                        capture_output=True, text=True, check=True,
                    )
                    self.assertEqual(authenticated.stdout, "1\n")
                finally:
                    subprocess.run(admin, input="DROP USER 'synthetic'@'%';",
                                   text=True, capture_output=True, check=True)

    def test_emits_one_aggregate_json_line(self):
        values = list(range(len(work_volume.FIELDS)))
        result = subprocess.CompletedProcess([], 0, "\t".join(map(str, values)) + "\n")
        with patch.object(work_volume.subprocess, "run", return_value=result) as run:
            with patch("sys.stdout") as stdout:
                self.assertEqual(work_volume.main(["--mysql-defaults-file", "/tmp/mysql.cnf",
                                                   "--database", "usnmocr"]), 0)
        output = "".join(call.args[0] for call in stdout.write.call_args_list)
        [line] = output.splitlines()
        record = json.loads(line)
        self.assertEqual(record["event"], "boinc_work_volume")
        self.assertEqual(record["schema_version"], 1)
        self.assertEqual(record["workunits"]["total"], values[0])
        self.assertEqual(record["workunits"]["need_validate"]["no"], values[1])
        self.assertEqual(record["workunits"]["need_validate"]["yes"], values[2])
        self.assertEqual(record["workunits"]["assimilate_state"]["ready"], values[4])
        self.assertEqual(record["workunits"]["file_delete_state"]["error"], values[10])
        self.assertEqual(record["results"]["total"], values[12])
        self.assertEqual(record["results"]["server_state"]["in_progress"], values[15])
        self.assertEqual(record["active_users"], values[18])
        self.assertEqual(record["active_hosts"], values[19])
        command = run.call_args.args[0]
        self.assertEqual(command[1], "--defaults-extra-file=/tmp/mysql.cnf")
        self.assertIn("FROM workunit", command[-1])
        self.assertIn("FROM result", command[-1])
        self.assertIn("expavg_credit > 1", command[-1])
        self.assertNotIn("SELECT *", command[-1].upper())
        self.assertNotIn("email_addr", command[-1])
        self.assertNotIn("hostid", command[-1])
        self.assertNotIn("IDENTITY_HEADER", line)
        for option in ("--ssl", "--ssl-verify-server-cert",
                       "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem"):
            self.assertIn(option, command)

    def test_empty_tables_emit_zero_counts(self):
        result = subprocess.CompletedProcess([], 0, "\t".join(["0"] * len(work_volume.FIELDS)))
        with patch.object(work_volume.subprocess, "run", return_value=result):
            with patch("sys.stdout") as stdout:
                self.assertEqual(work_volume.main(["--mysql-defaults-file", "/tmp/mysql.cnf",
                                                   "--database", "usnmocr"]), 0)
        record = json.loads("".join(call.args[0] for call in stdout.write.call_args_list))
        self.assertEqual(record["workunits"]["total"], 0)
        self.assertEqual(record["results"]["server_state"]["other"], 0)

    def test_database_failure_logs_no_raw_error_or_json(self):
        error = subprocess.CalledProcessError(1, ["mysql"], stderr="private database detail")
        with patch.object(work_volume.subprocess, "run", side_effect=error):
            with patch("sys.stdout") as stdout, patch("sys.stderr") as stderr:
                self.assertEqual(work_volume.main(["--mysql-defaults-file", "/tmp/mysql.cnf",
                                                   "--database", "usnmocr"]), 1)
        stdout.write.assert_not_called()
        self.assertNotIn("private database detail", stderr.write.call_args.args[0])

    def test_invalid_database_name_is_rejected(self):
        with patch.object(work_volume.subprocess, "run") as run:
            with patch("sys.stderr"):
                self.assertEqual(work_volume.main(["--mysql-defaults-file", "/tmp/mysql.cnf",
                                                   "--database", "usnmocr;select"]), 2)
        run.assert_not_called()

    def test_workbook_queries_parse_the_export_schema_and_chart_samples(self):
        workbook = json.loads((ROOT / "infra/workbooks/boinc.json").read_text())
        items = {item["name"]: item["content"] for item in workbook["items"]}
        latest = items["workvolume-latest"]["query"]
        states = items["workvolume-states"]["query"]
        chart = items["workvolume-chart"]["query"]
        for query in (latest, states, chart):
            self.assertIn(
                "union isfuzzy=true ContainerAppConsoleLogs, ContainerAppConsoleLogs_CL",
                query,
            )
            self.assertIn('column_ifexists("ContainerAppName", "")', query)
            self.assertIn('column_ifexists("ContainerAppName_s", "")', query)
            self.assertIn('column_ifexists("Log", "")', query)
            self.assertIn('column_ifexists("Log_s", "")', query)
            self.assertIn("| distinct TimeGenerated, App, Msg", query)
            self.assertIn("parse_json(Msg)", query)
            self.assertIn('tostring(m.event) == "boinc_work_volume"', query)
            self.assertIn("m.workunits.assimilate_state.ready", query)
            self.assertIn("m.results.server_state.in_progress", query)
            self.assertIn("m.active_users", query)
            self.assertIn("m.active_hosts", query)
        for query in (latest, states):
            self.assertIn("let latestSample = samples | top 1 by TimeGenerated desc;", query)
            self.assertIn("\nlatestSample\n", query)
            self.assertNotIn("let latest =", query)
        for state in ("need_validate.no", "need_validate.yes", "assimilate_state.init",
                      "assimilate_state.ready", "assimilate_state.done", "assimilate_state.other",
                      "file_delete_state.init", "file_delete_state.ready", "file_delete_state.done",
                      "file_delete_state.error", "file_delete_state.other", "server_state.inactive",
                      "server_state.unsent", "server_state.in_progress", "server_state.over",
                      "server_state.other"):
            self.assertIn(state, states)
        self.assertIn("bin(TimeGenerated, {TimeRange:grain})", chart)

        system_queries = [
            items[name]["query"]
            for name in ("executions", "executionsbyday", "revisions", "restarts", "warnings")
        ]
        for query in system_queries:
            self.assertIn(
                "union isfuzzy=true ContainerAppSystemLogs, ContainerAppSystemLogs_CL",
                query,
            )
            for column in (
                "ContainerAppName", "JobName", "RevisionName", "ReplicaName",
                "Reason", "Type", "Log", "ContainerAppName_s", "JobName_s",
                "RevisionName_s", "ReplicaName_s", "Reason_s", "Type_s", "Log_s",
            ):
                self.assertIn(f'column_ifexists("{column}"', query)
            self.assertIn(
                "| distinct TimeGenerated, App, Revision, Replica, Reason, Kind, Msg",
                query,
            )

        self.assertIn('App contains "-caj-"', items["executions"]["query"])
        self.assertIn('App contains "-ca-"', items["revisions"]["query"])
        self.assertNotIn("ExecutionName", items["executions"]["query"])
        self.assertEqual(items["executions"]["title"], "Job activity by last event")
        self.assertIn("by Job = App", items["executions"]["query"])
        self.assertIn("LastReason = Reason", items["executions"]["query"])
        self.assertIn("Events = count()", items["executionsbyday"]["query"])
        self.assertEqual(
            items["executionsbyday"]["title"],
            "Job system events per day, by reason",
        )


@unittest.skipUnless(os.environ.get("USNM_WORK_VOLUME_MYSQL_PORT"),
                     "set USNM_WORK_VOLUME_MYSQL_PORT for the isolated MySQL fixture")
class WorkVolumeSQL(unittest.TestCase):
    # Only the columns read by QUERY, with types from BOINC 22bbebe0 db/schema.sql.
    # Temporary tables disappear on disconnect; no durable schema is modified.
    SCHEMA = """
CREATE TEMPORARY TABLE workunit (
  need_validate SMALLINT NOT NULL, assimilate_state INTEGER NOT NULL,
  file_delete_state INTEGER NOT NULL);
CREATE TEMPORARY TABLE result (server_state INTEGER NOT NULL);
CREATE TEMPORARY TABLE `user` (expavg_credit DOUBLE NOT NULL);
CREATE TEMPORARY TABLE host (expavg_credit DOUBLE NOT NULL);
"""

    def query(self, fixture=""):
        result = subprocess.run(
            ["mysql", "--no-defaults", "--protocol=TCP", "--host=127.0.0.1",
             f"--port={os.environ['USNM_WORK_VOLUME_MYSQL_PORT']}", "--user=root",
             "--database=mysql", "--batch", "--raw", "--skip-column-names"],
            input=self.SCHEMA + fixture + work_volume.QUERY + ";",
            capture_output=True, text=True, check=True,
        )
        [line] = result.stdout.splitlines()
        return list(map(int, line.split("\t")))

    def test_empty_tables(self):
        self.assertEqual(self.query(), [0] * len(work_volume.FIELDS))

    def test_all_states_and_recent_credit_threshold(self):
        values = self.query("""
INSERT INTO workunit VALUES (0,0,0),(1,1,1),(1,2,2),(0,9,3),(1,1,9);
INSERT INTO result VALUES (1),(2),(4),(5),(9),(0),(4);
INSERT INTO `user` VALUES (0),(1),(1.01),(2);
INSERT INTO host VALUES (-1),(1),(1.01);
""")
        self.assertEqual(values, [5, 2, 3, 1, 2, 1, 1, 1, 1, 1, 1, 1,
                                  7, 1, 1, 2, 1, 2, 2, 1])
        self.assertEqual(work_volume.payload(values)["results"]["total"], 7)


if __name__ == "__main__":
    unittest.main()
