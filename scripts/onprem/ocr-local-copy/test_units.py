"""Checks on the OCR output copy's units: python3 -m unittest -v
scripts/onprem/ocr-local-copy/test_units.py. The files are read as text and
compared with the command line, so a flag renamed in one place fails here."""

import os
import re
import shlex
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
CLI = os.path.join(ROOT, "crates", "usnm-ingest", "src", "cli", "ocr_local_copy.rs")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def service():
    values = {}
    for raw in read(os.path.join(HERE, "usnm-ocr-local-copy.service")).splitlines():
        line = raw.strip()
        if line and not line.startswith(("#", "[")):
            key, _, value = line.partition("=")
            values.setdefault(key, []).append(value)
    return values


class ServiceTest(unittest.TestCase):
    def setUp(self):
        self.unit = service()
        self.words = shlex.split(self.unit["ExecStart"][0])

    def test_runs_the_subcommand_with_named_flags_that_exist(self):
        self.assertEqual(self.words[:2], ["/usr/local/bin/usnm-ingest", "ocr-local-copy"])
        source = read(CLI)
        flags = {name.replace("_", "-") for name in re.findall(r"#\[arg\([^\]]*\)\]\s+(\w+):", source)}
        args = self.words[2:]
        self.assertTrue(args and len(args) % 2 == 0, args)
        for flag in args[::2]:
            self.assertTrue(flag.startswith("--"), flag)
            self.assertIn(flag[2:], flags)
        self.assertNotIn("--allow-unmounted", args)

    def test_writes_only_the_dataset_and_its_state_directory(self):
        to = self.words[self.words.index("--to") + 1]
        self.assertEqual(self.unit["ReadWritePaths"], [to])
        self.assertEqual(self.unit["RequiresMountsFor"], [to])
        self.assertEqual(self.unit["ProtectSystem"], ["strict"])
        status = self.words[self.words.index("--status") + 1]
        self.assertTrue(status.startswith("/var/lib/" + self.unit["StateDirectory"][0] + "/"))

    def test_a_failure_is_never_skipped_quietly(self):
        for key in self.unit:
            self.assertFalse(key.startswith(("Condition", "Assert")), key)

    def test_the_lake_url_comes_from_the_setting_the_command_reads(self):
        self.assertIn('env = "USNM_OCR_LOCAL_COPY_FROM"', read(CLI))
        self.assertIn("USNM_OCR_LOCAL_COPY_FROM", read(os.path.join(HERE, "usnm-ocr-local-copy.service")))


if __name__ == "__main__":
    unittest.main()
