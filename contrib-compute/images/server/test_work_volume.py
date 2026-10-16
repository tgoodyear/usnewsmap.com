"""Tests for the BOINC work-volume JSON export and its workbook consumers."""

import importlib.util
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("work_volume", HERE / "work-volume.py")
work_volume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(work_volume)

ROOT = HERE.parents[2]


class WorkVolume(unittest.TestCase):
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
            self.assertIn("ContainerAppConsoleLogs_CL", query)
            self.assertIn("parse_json(Msg)", query)
            self.assertIn('tostring(m.event) == "boinc_work_volume"', query)
            self.assertIn("m.workunits.assimilate_state.ready", query)
            self.assertIn("m.results.server_state.in_progress", query)
            self.assertIn("m.active_users", query)
            self.assertIn("m.active_hosts", query)
        self.assertIn("| top 1 by TimeGenerated desc", latest)
        for state in ("need_validate.no", "need_validate.yes", "assimilate_state.init",
                      "assimilate_state.ready", "assimilate_state.done", "assimilate_state.other",
                      "file_delete_state.init", "file_delete_state.ready", "file_delete_state.done",
                      "file_delete_state.error", "file_delete_state.other", "server_state.inactive",
                      "server_state.unsent", "server_state.in_progress", "server_state.over",
                      "server_state.other"):
            self.assertIn(state, states)
        self.assertIn("bin(TimeGenerated, {TimeRange:grain})", chart)


if __name__ == "__main__":
    unittest.main()
