"""Regression checks for the server's daemon configuration."""

from pathlib import Path
import os
import shlex
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET


class DaemonConfig(unittest.TestCase):
    def test_feeder_interleaves_all_apps_with_expanded_shmem(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [shlex.split(cmd.text) for cmd in root.findall("./daemons/daemon/cmd")]
        self.assertEqual(
            [cmd for cmd in commands if cmd[0] == "feeder"],
            [["feeder", "-d", "2", "--allapps"]],
        )
        self.assertEqual(root.findtext("./config/shmem_work_items"), "1000")

    def test_matching_runtime_links_are_not_removed_or_replaced(self):
        root = Path(__file__).resolve().parents[2]
        source = (root / "contrib-compute/images/server/lib.sh").read_text()
        links = source.split('  if [ "$(readlink "$PROJ/config.xml"', 1)[1]
        links = 'if [ "$(readlink "$PROJ/config.xml"' + links.split("\n}", 1)[0]
        with tempfile.TemporaryDirectory(dir=root) as directory:
            work = Path(directory)
            project = work / "project"
            secrets = work / "secrets"
            project.mkdir()
            secrets.mkdir()
            (secrets / "keys").mkdir()
            (secrets / "keys/upload_public").write_text("public fixture")
            (secrets / "config.xml").write_text("<config/>")
            (project / "keys").symlink_to(secrets / "keys")
            (project / "config.xml").symlink_to(secrets / "config.xml")
            before = [(project / name).lstat().st_ino for name in ("keys", "config.xml")]
            subprocess.run(
                ["sh", "-eu", "-c", links], check=True,
                env={**os.environ, "PROJ": str(project), "SECRETS": str(secrets)},
            )
            self.assertEqual(before, [(project / name).lstat().st_ino for name in ("keys", "config.xml")])
            self.assertEqual((secrets / "keys/upload_public").read_text(), "public fixture")

    def test_db_purge_runs_continuously_with_thirty_day_retention(self):
        root = ET.parse(Path(__file__).with_name("config.xml.template"))
        commands = [
            shlex.split(cmd.text)
            for cmd in root.findall("./daemons/daemon/cmd")
        ]
        purge = [cmd for cmd in commands if cmd[0] == "db_purge"]
        self.assertEqual(purge, [["db_purge", "-d", "2", "--min_age_days", "30"]])
