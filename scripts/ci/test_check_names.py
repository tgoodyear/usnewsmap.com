"""Tests for scripts/ci/check-names.py (python3 -m unittest scripts/ci/test_check_names.py).
The checks themselves, on hand-made tables; CI also runs the script on the
real ones."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("check_names", os.path.join(HERE, "check-names.py"))
cn = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cn)

WORDS = ["api", "core", "data", "ingest", "ocr", "scratch", "state", "qw"]
ABBRS = {
    "ca": {"form": "hyphenated", "maxLength": 31},
    "caj": {"form": "hyphenated", "maxLength": 31},
    "cosno": {"form": "global", "maxLength": 44},
    "st": {"form": "compact", "maxLength": 24},
    "title": {"form": "title", "maxLength": 260},
    "shortName": {"form": "shortName", "maxLength": 12},
    "scopeEnv": {"form": "empty", "maxLength": 0},
    "rg": {"form": "hyphenated", "maxLength": 90},
}


def ok(value, abbr, env="prod", sfx="abc123"):
    return cn.conforms(value, abbr, env, sfx, WORDS, ABBRS)


class Conforms(unittest.TestCase):
    def test_hyphenated(self):
        self.assertIsNone(ok("usnm-ingest-caj-prod", "caj"))
        self.assertIsNone(ok("usnm-qw-cmp-0-ca-xabcd", "ca", env="xabcd"))

    def test_global_and_compact(self):
        self.assertIsNone(ok("usnm-state-cosno-dev-abc123", "cosno", env="dev"))
        self.assertIsNone(ok("usnmscratchstxabcdabc123", "st", env="xabcd"))

    def test_legacy_forms_fail(self):
        self.assertIn("usnm-", ok("caj-usnm-ingest-prod", "caj"))
        self.assertIsNotNone(ok("stusnmdabc123", "st"))
        self.assertIsNotNone(ok("cosmos-usnm-prod-abc123", "cosno"))

    def test_env_last_and_abbr_before_it(self):
        self.assertIn("end with", ok("usnm-ingest-caj-prod-1", "caj"))
        self.assertIn("token before", ok("usnm-ingest-ca-prod", "caj"))

    def test_registry_and_qualifiers(self):
        self.assertIn("registry", ok("usnm-ingestion-caj-prod", "caj"))
        self.assertIn("qualifiers", ok("usnm-api-a-b-c-d-ca-prod", "ca"))
        self.assertIn("registry", ok("usnmstuffstprodabc123", "st"))

    def test_length(self):
        self.assertIn("over the 31", ok("usnm-ingest-very-long-name-caj-prod", "caj"))

    def test_display_forms(self):
        self.assertIsNone(ok("usnm API (dev)", "title", env="dev"))
        self.assertIsNotNone(ok("usnewsmap API", "title"))
        self.assertIsNotNone(ok("usnm API (prod)", "title", env="dev"))
        self.assertIsNone(ok("usnm-xabcd", "shortName", env="xabcd"))
        self.assertIsNotNone(ok("usnm", "shortName"))
        self.assertIsNone(ok("", "scopeEnv"))
        self.assertIsNotNone(ok("/prod", "scopeEnv"))


TABLES = {
    "nameTypes": {"apiApp": "ca", "rg": "rg", "nic": "rg", "tiles": "st"},
    "functionWords": WORDS,
    "typeAbbreviations": ABBRS,
    "createOnlyKeys": ["nic"],
}


class CheckNames(unittest.TestCase):
    def test_conventional_map(self):
        names = {"apiApp": "usnm-api-ca-dev", "rg": "usnm-core-rg-dev", "nic": "usnm-core-x-rg-dev",
                 "tiles": "stusnmtabc123"}
        self.assertEqual(cn.check_names(names, "dev", "abc123", False, TABLES,
                                        {"tiles": {"scope": "all"}}), [])
        # A prod-only exception doesn't cover the conventional form.
        out = cn.check_names(names, "dev", "abc123", False, TABLES, {"tiles": {}})
        self.assertEqual(len(out), 1)
        self.assertIn("tiles", out[0])

    def test_pinned_needs_exceptions_and_drops_stale_ones(self):
        names = {"apiApp": "ca-usnm-prod", "rg": "usnm-core-rg-prod", "nic": "", "tiles": "stusnmtffapzy"}
        out = cn.check_names(names, "prod", "ffapzy", True, TABLES, {"tiles": {}})
        self.assertEqual(len(out), 1)
        self.assertIn("apiApp", out[0])
        out = cn.check_names(names, "prod", "ffapzy", True, TABLES,
                             {"apiApp": {}, "rg": {}, "tiles": {}})
        self.assertEqual(len(out), 1)
        self.assertIn("remove its exception", out[0])

    def test_keys_match_the_types(self):
        out = cn.check_names({"apiApp": "usnm-api-ca-dev"}, "dev", "abc123", False, TABLES, {})
        self.assertIn("keys and nameTypes differ", out[0])


class Prod(unittest.TestCase):
    def test_a_rename_needs_prod_json(self):
        self.assertEqual(cn.check_prod({"rg": "rg-usnm-prod"}, {"$comment": "x", "rg": "rg-usnm-prod"}), [])
        out = cn.check_prod({"rg": "usnm-core-rg-prod"}, {"rg": "rg-usnm-prod"})
        self.assertIn("prod.json", out[0])


class Exceptions(unittest.TestCase):
    GOOD = {"reason": "r", "adr": "ADR-0017, Exceptions", "until": "phase-2"}

    def test_fields_and_order(self):
        self.assertEqual(cn.check_exceptions({"a": self.GOOD, "b": {**self.GOOD, "until": "#309"}}, None), [])
        out = cn.check_exceptions({"b": self.GOOD, "a": {"until": "later"}}, None)
        self.assertTrue(any("sorted" in o for o in out))
        self.assertTrue(any("no reason" in o for o in out))
        self.assertTrue(any("isn't phase" in o for o in out))

    def test_only_shrinks(self):
        out = cn.check_exceptions({"a": self.GOOD, "b": self.GOOD}, {"a": self.GOOD})
        self.assertEqual(len(out), 1)
        self.assertIn("exception b is new", out[0])


class Sources(unittest.TestCase):
    def check(self, text):
        with tempfile.TemporaryDirectory(dir=cn.ROOT) as d:
            p = Path(d) / "m.bicep"
            p.write_text(text)
            return cn.check_sources([p])

    def test_a_name_built_in_a_module(self):
        out = self.check("resource x 'T' = {\n  name: 'caj-usnm-ingest-${env}'\n}\n")
        self.assertEqual(len(out), 1)
        self.assertIn("names()", out[0])
        self.assertEqual(len(self.check("  displayName: 'usnewsmap API'\n")), 1)
        self.assertEqual(len(self.check("    groupShortName: 'usnm'\n")), 1)
        out = self.check("  { name: 'pe-usnm-blob', target: storageId, description: 'x' }\n")
        self.assertEqual(len(out), 1)
        self.assertEqual(self.check("  { name: e.name, description: 'usnm-api answered' }\n"), [])

    def test_parameters_children_and_comments_pass(self):
        self.assertEqual(self.check("  name: name\n  name: 'usnm'\n  // name: 'usnm-x'\n"
                                    "  { name: 'USNM_BACKEND', value: 'x' }\n"), [])

    def test_guid_seeds(self):
        self.assertEqual(self.check("  name: guid(resourceGroup().id, 'usnm-swa-deployer')\n"), [])
        out = self.check("  name: guid(resourceGroup().id, 'usnm-new-seed')\n")
        self.assertIn("frozen seed", out[0])

    def test_a_name_split_over_lines(self):
        # The value on the next line, a ternary over three lines, and a
        # concatenation continued on the next line: each still counts, at
        # the property's own line.
        out = self.check("resource x 'T' = {\n  name:\n    'caj-usnm-ingest-${env}'\n}\n")
        self.assertEqual(len(out), 1)
        self.assertTrue(out[0].endswith("names() in infra/naming.bicep (ADR-0017)"))
        self.assertIn("m.bicep:2:", out[0])
        out = self.check("resource x 'T' = {\n  name: pinned\n    ? 'ca-usnm-${env}'\n    : n.apiApp\n"
                         "  location: location\n}\n")
        self.assertEqual(len(out), 1)
        self.assertIn("m.bicep:2:", out[0])
        out = self.check("resource x 'T' = {\n  displayName: concat(\n    'usnewsmap ',\n    env)\n}\n")
        self.assertEqual(len(out), 1)
        # The next property isn't read as part of the value.
        self.assertEqual(self.check("resource x 'T' = {\n  name: n.apiApp\n  tags: { a: 'usnm-x' }\n}\n"), [])

    def test_a_guid_call_split_over_lines(self):
        out = self.check("resource r 'T' = {\n  name: guid(\n    resourceGroup().id,\n"
                         "    'usnm-new-seed'\n  )\n  properties: {}\n}\n")
        self.assertEqual(len(out), 1)
        self.assertIn("m.bicep:4: guid() seed 'usnm-new-seed'", out[0])
        self.assertEqual(self.check("resource r 'T' = {\n  name: guid(\n    resourceGroup().id,\n"
                                    "    'usnm-swa-deployer'\n  )\n  properties: { scope: '/dbs/usnm' }\n}\n"),
                         [])

    def test_comments_are_skipped_but_strings_kept(self):
        self.assertEqual(self.check("  name: n.x // was 'caj-usnm-x'\n  /* name: 'ca-usnm-y'\n"
                                    "  name: 'ca-usnm-z' */\n"), [])
        out = self.check("  name: 'https://ca-usnm-x' // a comment\n")
        self.assertEqual(len(out), 1)


class Base(unittest.TestCase):
    """--base and --require-base: a base that can't be read is a problem."""

    def test_no_base(self):
        self.assertEqual(cn.base_exceptions("", False), (None, []))
        base, out = cn.base_exceptions("", True)
        self.assertIsNone(base)
        self.assertIn("--require-base", out[0])

    def test_a_revision_that_doesnt_resolve(self):
        base, out = cn.base_exceptions("refs/heads/no-such-branch-309", False)
        self.assertIsNone(base)
        self.assertIn("isn't a revision here", out[0])

    def test_the_current_revision(self):
        # HEAD has both files (in a checkout with history at all).
        if cn.git("cat-file", "-e", "HEAD:infra/names/exceptions.json").returncode != 0:
            self.skipTest("no committed exceptions.json at HEAD")
        base, out = cn.base_exceptions("HEAD", True)
        self.assertEqual(out, [])
        self.assertIsInstance(base, dict)


if __name__ == "__main__":
    unittest.main()
