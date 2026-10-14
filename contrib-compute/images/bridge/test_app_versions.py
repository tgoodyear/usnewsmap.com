"""Tests for app_versions.py: the checks and the placing, with a stand-in for crypt_prog.

The stand-in accepts a signature whose text is "good <md5 of the file>", so a .sig for another
file, or a changed file, fails as it would with BOINC's crypt_prog -verify."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


av = load("app_versions", HERE / "app_versions.py")
install = load("boinc_install", ROOT / "scripts/boinc/install.py")

CRYPT_PROG = """#!/usr/bin/env python3
import hashlib, sys
op, f, sig, key = sys.argv[1:5]
assert op == "-verify"
open(key).read()
ok = open(sig).read().strip() == "good " + hashlib.md5(open(f, "rb").read()).hexdigest()
print("signature is valid" if ok else "signature is invalid")
sys.exit(0 if ok else 1)
"""

VERSION_XML = """<version>
  <file><physical_name>docker_wrapper_1</physical_name><main_program /></file>
  <file><physical_name>Dockerfile_1</physical_name><logical_name>Dockerfile</logical_name><copy_file /></file>
  <is_wrapper />
</version>"""


class AppVersions(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.crypt = self.root / "crypt_prog"
        self.crypt.write_text(CRYPT_PROG)
        self.crypt.chmod(0o755)
        self.key = self.root / "code_sign_public"
        self.key.write_text("1024\nabc\n.\n")
        self.stage = self.root / "stage"
        self.stage.mkdir()
        (self.stage / "version.xml").write_text(VERSION_XML)
        for name, body in (("docker_wrapper_1", b"\x7fELF wrapper"), ("Dockerfile_1", b"FROM x@sha256:1\n")):
            (self.stage / name).write_bytes(body)
            self.sign(name)

    def tearDown(self):
        self.tmp.cleanup()

    def sign(self, name, text=None):
        digest = hashlib.md5((self.stage / name).read_bytes()).hexdigest()
        (self.stage / (name + ".sig")).write_text(text or f"good {digest}\n")

    def check(self):
        return av.check(self.stage, self.key, str(self.crypt))

    def test_a_signed_version_passes(self):
        self.assertEqual(self.check(), ["Dockerfile_1", "docker_wrapper_1"])

    def test_refuses_an_unsigned_file(self):
        (self.stage / "job_1.toml").write_text("x")
        with self.assertRaisesRegex(av.Refused, "no .sig for job_1.toml"):
            self.check()

    def test_refuses_a_bad_signature_or_a_changed_file(self):
        self.sign("Dockerfile_1", "good 0000")
        with self.assertRaisesRegex(av.Refused, "Dockerfile_1.sig does not verify"):
            self.check()
        self.sign("Dockerfile_1")
        (self.stage / "docker_wrapper_1").write_bytes(b"changed")
        with self.assertRaisesRegex(av.Refused, "docker_wrapper_1.sig does not verify"):
            self.check()

    def test_refuses_without_the_public_key(self):
        self.key.unlink()
        with self.assertRaisesRegex(av.Refused, "no code-signing public key"):
            self.check()

    def test_refuses_an_orphan_sig_a_missing_version_xml_and_unnamed_files(self):
        (self.stage / "other.sig").write_text("good x")
        with self.assertRaisesRegex(av.Refused, "a .sig with no file: other.sig"):
            self.check()
        (self.stage / "other.sig").unlink()
        (self.stage / "version.xml").write_text(VERSION_XML.replace("Dockerfile_1", "Missing_1"))
        with self.assertRaisesRegex(av.Refused, "names files that were not uploaded: Missing_1"):
            self.check()
        (self.stage / "version.xml").unlink()
        with self.assertRaisesRegex(av.Refused, "no version.xml"):
            self.check()

    def test_refuses_a_directory(self):
        (self.stage / "sub").mkdir()
        with self.assertRaisesRegex(av.Refused, "not a plain file name"):
            self.check()

    def test_place_is_atomic_idempotent_and_refuses_a_change(self):
        target = self.root / "project/apps/usnm_gold/1.0/x86_64-pc-linux-gnu__docker"
        self.assertTrue(av.place(self.stage, target))
        self.assertEqual(sorted(p.name for p in target.iterdir()), sorted(p.name for p in self.stage.iterdir()))
        self.assertEqual((target / "docker_wrapper_1").stat().st_mode & 0o777, 0o755)
        self.assertEqual([p.name for p in target.parent.iterdir()], [target.name])
        self.assertFalse(av.place(self.stage, target))
        (self.stage / "Dockerfile_1").write_bytes(b"FROM other\n")
        with self.assertRaisesRegex(av.Refused, "exists with other files"):
            av.place(self.stage, target)

    def test_templates_match_install_py(self):
        self.assertEqual(av.TEMPLATES, install.TEMPLATES)
        for names in av.TEMPLATES.values():
            for n in names:
                self.assertTrue((ROOT / "ocr/boinc" / n).is_file(), n)

    def test_staged_install_py_output_is_what_check_reads(self):
        wrapper = self.root / "docker_wrapper"
        wrapper.write_bytes(b"wrapper")
        out = install.stage(self.root / "s2", wrapper, "ghcr.io/x/y@sha256:" + "a" * 64, "1.0", "usnm_gold")
        self.stage = out
        for p in list(out.iterdir()):
            if p.name != "version.xml":
                self.sign(p.name)
        self.assertEqual(self.check(), ["Dockerfile_1", "docker_wrapper_1", "job_1.toml"])


if __name__ == "__main__":
    unittest.main()
