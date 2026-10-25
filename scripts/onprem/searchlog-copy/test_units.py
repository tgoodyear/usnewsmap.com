"""Checks on the search log copy's units (#1097): python3 -m unittest -v
scripts/onprem/searchlog-copy/test_units.py. systemd is not needed and nothing
is installed or started: the files are read as text and compared with each
other, with the command line in crates/usnm-ingest/src/cli/searchlog_copy.rs
and with the README, so a flag renamed in one place fails here."""

import os
import re
import shlex
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
CLI = os.path.join(ROOT, "crates", "usnm-ingest", "src", "cli", "searchlog_copy.rs")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def unit(name):
    """A unit file as {section: {key: [values]}}; a key may repeat, as Environment= does."""
    sections, current = {}, None
    for raw in read(os.path.join(HERE, name)).splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        header = re.fullmatch(r"\[(\w+)\]", line)
        if header:
            current = sections.setdefault(header.group(1), {})
        else:
            key, _, value = line.partition("=")
            current.setdefault(key, []).append(value)
    return sections


def cli_flags():
    """The long flags and the environment variables of the subcommand, from its clap fields."""
    source = read(CLI).split("#[cfg(test)]")[0]
    fields = re.findall(r"#\[arg\(([^\]]*)\)\]\s+(?:pub )?(\w+):", source)
    flags = {name.replace("_", "-") for _, name in fields}
    envs = set(re.findall(r'env = "(\w+)"', source))
    return flags, envs


class ServiceTest(unittest.TestCase):
    def setUp(self):
        self.unit = unit("usnm-searchlog-copy.service")
        self.service = self.unit["Service"]
        self.words = shlex.split(self.service["ExecStart"][0])

    def test_runs_the_subcommand_with_named_flags_that_exist(self):
        self.assertEqual(self.words[:2], ["/usr/local/bin/usnm-ingest", "searchlog-copy"])
        args = self.words[2:]
        flags, _ = cli_flags()
        self.assertTrue(args and len(args) % 2 == 0, args)
        for flag, value in zip(args[::2], args[1::2]):
            self.assertTrue(flag.startswith("--"), "positional argument %r" % flag)
            self.assertIn(flag[2:], flags, flag)
            self.assertFalse(value.startswith("--"), value)
        self.assertNotIn("--allow-unmounted", args, "the real copy needs the mount")

    def test_settings_the_unit_leaves_to_the_env_file_are_the_commands(self):
        _, envs = cli_flags()
        env_names = {v.split("=")[0] for v in self.service["Environment"]}
        # The three options every usnm-ingest command takes are set in the unit ...
        self.assertTrue({"USNM_CURATED_URL", "USNM_REFERENCE_URL", "USNM_STATE_FILE"} <= env_names)
        # ... and the lake's URL is in the env file, under the name the command reads.
        self.assertIn("USNM_SEARCH_LOG_URL", envs)
        self.assertIn("USNM_SEARCH_LOG_URL", read(os.path.join(HERE, "usnm-searchlog-copy.service")))
        self.assertEqual(self.service["EnvironmentFile"], ["/etc/usnm/searchlog-copy.env"])

    def test_the_dataset_is_mounted_and_the_only_place_written(self):
        to = self.words[self.words.index("--to") + 1]
        self.assertEqual(self.unit["Unit"]["RequiresMountsFor"], [to])
        self.assertEqual(self.service["ReadWritePaths"], [to])
        self.assertEqual(self.service["ProtectSystem"], ["strict"])
        # The status file is written in the state directory, outside the dataset.
        status = self.words[self.words.index("--status") + 1]
        state_dir = "/var/lib/" + self.service["StateDirectory"][0]
        self.assertTrue(status.startswith(state_dir + "/"), status)
        self.assertFalse(status.startswith(to), "status is not in the dataset")

    def test_a_dataset_that_is_not_mounted_fails_the_unit(self):
        # A failed AssertPathIsMountPoint= fails only the start job: the unit stays "inactive" and
        # is not in `systemctl --failed`. A failed Condition*= skips the unit without a trace.
        # So the mount is checked by the command, which exits 1 (and the journal has the JSON
        # failure line) unless it is given --allow-unmounted, which the unit does not pass.
        for section in (self.unit["Unit"], self.service):
            for key in section:
                self.assertNotRegex(key, r"^(Assert|Condition)PathIsMountPoint$")
        self.assertNotIn("--allow-unmounted", self.words)
        self.assertIn("require_mount: !allow_unmounted", read(CLI))

    def test_a_missing_or_unexecutable_binary_fails_the_unit(self):
        # A failed Condition*= (ConditionFileIsExecutable=, say) skips every timer activation
        # quietly: `systemctl start` returns 0 with Result=success and ConditionResult=no, the
        # status file stays as it was and nothing says the copy has stopped. A failed Assert*=
        # only fails the start job. Left to ExecStart=, a binary that is missing or has lost its
        # executable bit fails with status=203/EXEC and the unit is in `systemctl --failed`
        # (checked on systemd 257). So no unit here may skip or assert its way out of a run, and
        # ExecStart= may not carry a prefix that ignores its failure.
        for section in (self.unit["Unit"], self.service):
            for key in section:
                self.assertFalse(key.startswith(("Condition", "Assert")), key)
                self.assertNotEqual(key, "ExecCondition")
        self.assertNotIn("ExecStartPre", self.service)
        self.assertTrue(self.service["ExecStart"][0].startswith("/"), self.service["ExecStart"])

    def test_search_text_cannot_rest_in_a_temporary_directory(self):
        self.assertEqual(self.service["PrivateTmp"], ["yes"])
        self.assertNotIn("USNM_WORK_DIR", "".join(self.service["Environment"]))

    def test_is_confined_and_runs_as_its_own_user_with_the_arc_token_group(self):
        s = self.service
        self.assertEqual(s["User"], ["usnm-searchlog"])
        self.assertEqual(s["SupplementaryGroups"], ["himds"])
        self.assertEqual(s["NoNewPrivileges"], ["yes"])
        self.assertEqual(s["CapabilityBoundingSet"], [""])
        self.assertEqual(s["Type"], ["oneshot"])
        self.assertEqual(s["PrivateDevices"], ["yes"])
        self.assertEqual(s["ProtectHome"], ["yes"])


class TimerTest(unittest.TestCase):
    def setUp(self):
        self.timer = unit("usnm-searchlog-copy.timer")["Timer"]

    def test_runs_daily_in_utc_after_the_day_file_is_written(self):
        (calendar,) = self.timer["OnCalendar"]
        match = re.fullmatch(r"\*-\*-\* (\d\d):(\d\d):00 UTC", calendar)
        self.assertTrue(match, calendar)
        # The API writes a day an hour after it ends (ADR-0012), checked hourly.
        self.assertGreaterEqual(int(match.group(1)) * 60 + int(match.group(2)), 2 * 60)
        self.assertEqual(self.timer["Persistent"], ["true"])

    def test_names_no_other_unit(self):
        self.assertNotIn("Unit", self.timer)


class ReadmeTest(unittest.TestCase):
    def test_lists_every_file_here(self):
        readme = read(os.path.join(HERE, "README.md"))
        for name in sorted(os.listdir(HERE)):
            if name in ("README.md", "__pycache__"):
                continue
            self.assertIn("`%s`" % name, readme, "%s is not in the README's file table" % name)


if __name__ == "__main__":
    unittest.main()
