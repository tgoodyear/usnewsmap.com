"""Regression checks for the server's daemon configuration."""

from pathlib import Path
import shlex
import unittest
import xml.etree.ElementTree as ET


class DaemonConfig(unittest.TestCase):
    def test_db_purge_runs_continuously_with_thirty_day_retention(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [
            shlex.split(cmd.text)
            for cmd in root.findall("./daemons/daemon/cmd")
        ]
        purge = [cmd for cmd in commands if cmd[0] == "db_purge"]
        self.assertEqual(purge, [["db_purge", "-d", "2", "--min_age_days", "30"]])
