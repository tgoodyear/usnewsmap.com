"""Tests for check_appver.py on a real install.py staging, with a stand-in for crypt_prog that
accepts "good <md5 of the file>" as a signature (so a changed file fails, as with BOINC's)."""
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


ca = load("check_appver", HERE / "check_appver.py")
install = load("boinc_install", ROOT / "scripts/boinc/install.py")

CRYPT_PROG = """#!/usr/bin/env python3
import hashlib, sys
op, f, sig, key = sys.argv[1:5]
open(key).read()
ok = open(sig).read().strip() == "good " + hashlib.md5(open(f, "rb").read()).hexdigest()
sys.exit(0 if ok else 1)
"""


class CheckAppver(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.crypt = self.root / "crypt_prog"
        self.crypt.write_text(CRYPT_PROG)
        self.crypt.chmod(0o755)
        self.key = self.root / "code_sign_public"
        self.key.write_text("1024\nabc\n.\n")
        proj = self.root / "proj"
        proj.mkdir()
        (proj / "config.xml").write_text("<boinc/>")
        wrapper = self.root / "docker_wrapper"
        wrapper.write_bytes(b"wrapper")
        self.dir = install.install(proj, wrapper, "ghcr.io/x/y@sha256:" + "a" * 64, "1.0", "usnm_gold")
        self.sigs = self.root / "sigs"
        self.sigs.mkdir()
        for p in self.dir.iterdir():
            if p.name != "version.xml":
                self.sign(p.name)

    def tearDown(self):
        self.tmp.cleanup()

    def sign(self, name, text=None):
        digest = hashlib.md5((self.dir / name).read_bytes()).hexdigest()
        (self.sigs / (name + ".sig")).write_text(text or f"good {digest}\n")

    def check(self):
        return ca.check(self.dir, self.sigs, self.key, str(self.crypt))

    def test_a_signed_version_passes(self):
        self.assertEqual(
            self.check(),
            [
                "Dockerfile_usnm_gold_1.0_x86_64-pc-linux-gnu_docker",
                "docker_wrapper_usnm_gold_1.0_x86_64-pc-linux-gnu_docker",
                "job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml",
            ],
        )
        self.assertTrue((self.dir / "job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml.sig").is_file())

    def test_refuses_an_orphan_sig(self):
        (self.sigs / "extra_1.sig").write_text("good x")
        with self.assertRaisesRegex(ca.Refused, "a .sig with no staged file: extra_1.sig"):
            self.check()

    def test_refuses_a_missing_sig(self):
        (self.sigs / "job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml.sig").unlink()
        with self.assertRaisesRegex(ca.Refused, "no .sig for job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml"):
            self.check()

    def test_refuses_a_non_signature(self):
        (self.sigs / "notes.txt").write_text("x")
        with self.assertRaisesRegex(ca.Refused, "not a signature: notes.txt"):
            self.check()

    def test_refuses_a_bad_signature_or_changed_bytes(self):
        (self.dir / "Dockerfile_usnm_gold_1.0_x86_64-pc-linux-gnu_docker").write_text("FROM other\n")
        with self.assertRaisesRegex(ca.Refused, "Dockerfile_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.sig does not verify"):
            self.check()

    def test_refuses_version_xml_naming_a_missing_file(self):
        (self.dir / "job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml").unlink()
        (self.sigs / "job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml.sig").unlink()
        with self.assertRaisesRegex(ca.Refused, "version.xml names files that are not staged: job_usnm_gold_1.0_x86_64-pc-linux-gnu_docker.toml"):
            self.check()

    def test_refuses_without_a_public_key(self):
        self.key.unlink()
        with self.assertRaisesRegex(ca.Refused, "no code-signing public key"):
            self.check()

    def test_native_version_with_extra_files_passes_and_validates_kind(self):
        proj = self.root / "native_proj"
        proj.mkdir()
        (proj / "config.xml").write_text("<boinc/>")
        binary = self.root / "usnm-qwen"
        binary.write_bytes(b"native-qwen")
        llama = self.root / "llama-server"
        llama.write_bytes(b"llama-server")
        native_dir = install.install(
            proj,
            version="1.0",
            app="usnm_qwen",
            plan_class="apple_gpu_8b",
            platform="arm64-apple-darwin",
            kind="native",
            binary=binary,
            extra_files=[f"llama-server={llama}"],
        )
        native_sigs = self.root / "native_sigs"
        native_sigs.mkdir()
        for p in native_dir.iterdir():
            if p.name != "version.xml":
                digest = hashlib.md5(p.read_bytes()).hexdigest()
                (native_sigs / (p.name + ".sig")).write_text(f"good {digest}\n")
        with self.assertRaisesRegex(ca.Refused, "missing <is_wrapper/>"):
            ca.check(native_dir, native_sigs, self.key, str(self.crypt), kind="container")
        with self.assertRaisesRegex(ca.Refused, "must not contain <is_wrapper/>"):
            ca.check(self.dir, self.sigs, self.key, str(self.crypt), kind="native")
        self.assertEqual(
            ca.check(native_dir, native_sigs, self.key, str(self.crypt), kind="native"),
            [
                "usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b",
                "usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b_llama-server",
            ],
        )


if __name__ == "__main__":
    unittest.main()
