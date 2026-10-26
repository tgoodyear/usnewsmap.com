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
    def test_amd_identity_helper_is_signed_and_cannot_be_omitted(self):
        proj = self.root / "amd-project"
        proj.mkdir()
        (proj / "config.xml").write_text("<boinc/>")
        payload = self.root / "fixture"
        payload.write_bytes(b"native-fixture")
        directory = install.install(
            proj, version="1.0", app="usnm_qwen", plan_class="amd_vulkan_8b",
            kind="native", binary=payload,
            extra_files=[f"{name}={payload}" for name in
                         ("llama-server", "llama.lock", "weights.lock", "usnm-amd-device-map",
                          "libggml.so", "libggml-base.so", "libggml-vulkan.so")])
        signatures = self.root / "amd-sigs"
        signatures.mkdir()
        for path in directory.iterdir():
            if path.name != "version.xml":
                (signatures / (path.name + ".sig")).write_text("good " + hashlib.md5(path.read_bytes()).hexdigest())
        ca.check(directory, signatures, self.key, str(self.crypt), kind="native")
        for signature in directory.glob("*.sig"):
            signature.unlink()
        helper = next(p for p in directory.iterdir() if p.name.endswith("_usnm-amd-device-map"))
        helper.write_bytes(b"changed")
        with self.assertRaisesRegex(ca.Refused, "does not verify"):
            ca.check(directory, signatures, self.key, str(self.crypt), kind="native")
        for signature in directory.glob("*.sig"):
            signature.unlink()
        helper.write_bytes(payload.read_bytes())
        original_xml = (directory / "version.xml").read_bytes()
        for logical in ("usnm-amd-device-map", "libggml.so", "libggml-base.so", "libggml-vulkan.so"):
            tree = ca.ET.fromstring(original_xml)
            node = next(n for n in tree.findall("file") if n.findtext("logical_name") == logical)
            physical = directory / node.findtext("physical_name")
            content = physical.read_bytes()
            signature = signatures / (physical.name + ".sig")
            signature_content = signature.read_bytes()
            physical.unlink()
            signature.unlink()
            tree.remove(node)
            (directory / "version.xml").write_bytes(ca.ET.tostring(tree))
            with self.subTest(logical=logical), self.assertRaisesRegex(ca.Refused, "missing required bundle files: " + logical):
                ca.check(directory, signatures, self.key, str(self.crypt), kind="native")
            physical.write_bytes(content)
            signature.write_bytes(signature_content)
            (directory / "version.xml").write_bytes(original_xml)

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
        llama_lock = self.root / "llama.lock"
        llama_lock.write_text("tag = 'b5000'\n")
        weights_lock = self.root / "weights.lock"
        weights_lock.write_text("model = 'qwen3-vl'\n")
        native_dir = install.install(
            proj,
            version="1.0",
            app="usnm_qwen",
            plan_class="apple_gpu_8b",
            platform="arm64-apple-darwin",
            kind="native",
            binary=binary,
            extra_files=[
                f"llama-server={llama}",
                f"llama.lock={llama_lock}",
                f"weights.lock={weights_lock}",
            ],
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
                "usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b_llama.lock",
                "usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b_weights.lock",
            ],
        )

    def test_refuses_native_qwen_missing_required_bundle_files_or_windows_non_exe_wrapper(self):
        # Remove llama-server from a staged native usnm_qwen version directory and verify check_appver refuses it
        proj = self.root / "bad_qwen_proj"
        proj.mkdir()
        (proj / "config.xml").write_text("<boinc/>")
        binary = self.root / "usnm-qwen"
        binary.write_bytes(b"native-qwen")
        llama = self.root / "llama-server"
        llama.write_bytes(b"llama-server")
        llama_lock = self.root / "llama.lock"
        llama_lock.write_text("tag = 'b5000'\n")
        weights_lock = self.root / "weights.lock"
        weights_lock.write_text("model = 'qwen3-vl'\n")
        native_dir = install.install(
            proj,
            version="1.0",
            app="usnm_qwen",
            plan_class="apple_gpu_8b",
            platform="arm64-apple-darwin",
            kind="native",
            binary=binary,
            extra_files=[
                f"llama-server={llama}",
                f"llama.lock={llama_lock}",
                f"weights.lock={weights_lock}",
            ],
        )
        # Strip llama-server from disk and version.xml
        srv_file = native_dir / "usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b_llama-server"
        srv_file.unlink()
        spec = ca.ET.parse(native_dir / "version.xml").getroot()
        for f in list(spec.findall("file")):
            if f.findtext("logical_name") == "llama-server":
                spec.remove(f)
        ca.ET.ElementTree(spec).write(native_dir / "version.xml", encoding="unicode")
        sigs = self.root / "bad_qwen_sigs"
        sigs.mkdir()
        for p in native_dir.iterdir():
            if p.name != "version.xml":
                digest = hashlib.md5(p.read_bytes()).hexdigest()
                (sigs / (p.name + ".sig")).write_text(f"good {digest}\n")
        with self.assertRaisesRegex(ca.Refused, "missing required bundle files: llama-server"):
            ca.check(native_dir, sigs, self.key, str(self.crypt), kind="native")

        # Restore llama-server in version.xml and disk, then strip <api_version> to verify check_appver refuses it
        srv_file.write_bytes(b"llama-server")
        digest = hashlib.md5(srv_file.read_bytes()).hexdigest()
        (sigs / (srv_file.name + ".sig")).write_text(f"good {digest}\n")
        item = ca.ET.SubElement(spec, "file")
        ca.ET.SubElement(item, "physical_name").text = srv_file.name
        ca.ET.SubElement(item, "logical_name").text = "llama-server"
        ca.ET.SubElement(item, "copy_file")
        api_el = spec.find("api_version")
        if api_el is not None:
            spec.remove(api_el)
        ca.ET.ElementTree(spec).write(native_dir / "version.xml", encoding="unicode")
        with self.assertRaisesRegex(ca.Refused, "must specify <api_version> >= 6.0.0"):
            ca.check(native_dir, sigs, self.key, str(self.crypt), kind="native")


if __name__ == "__main__":
    unittest.main()
