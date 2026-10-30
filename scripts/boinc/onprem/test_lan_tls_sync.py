"""Exercise the real shell certificate checks with disposable certificates and no network."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).parent / "lan-tls/lan-tls-sync.sh"


@unittest.skipUnless(sys.platform.startswith("linux"), "the host script requires GNU date")
class LanTlsSyncTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory()
        cls.root = Path(cls.fixture.name)
        cls.addClassCleanup(cls.fixture.cleanup)
        cls.openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-keyout", "ca.key", "-out", "ca.pem", "-days", "365",
                    "-subj", "/CN=Disposable LAN TLS test CA")
        for name, host, days in (
            ("current", "boinc.usnewsmap.com", 120),
            ("rollback", "boinc.usnewsmap.com", 90),
            ("expiring", "boinc.usnewsmap.com", 1),
            ("wronghost", "other.example", 180),
            ("valid", "boinc.usnewsmap.com", 180),
        ):
            cls.openssl("req", "-new", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", name + ".key", "-out", name + ".csr",
                        "-subj", "/CN=" + host)
            (cls.root / "extensions").write_text(
                f"subjectAltName=DNS:{host}\nextendedKeyUsage=serverAuth\n")
            cls.openssl("x509", "-req", "-in", name + ".csr", "-CA", "ca.pem",
                        "-CAkey", "ca.key", "-CAcreateserial", "-out", name + ".pem",
                        "-days", str(days), "-extfile", "extensions")
        cls.openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-keyout", "untrusted.key", "-out", "untrusted.pem", "-days", "180",
                    "-subj", "/CN=boinc.usnewsmap.com",
                    "-addext", "subjectAltName=DNS:boinc.usnewsmap.com")

    @classmethod
    def openssl(cls, *args):
        subprocess.run(["openssl", *args], cwd=cls.root, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.work = Path(tmp.name)
        self.installed = self.work / "installed"
        self.installed.mkdir()
        shutil.copy(self.root / "current.key", self.installed / "privkey.pem")
        (self.installed / "fullchain.pem").write_text(
            (self.root / "current.pem").read_text() + (self.root / "ca.pem").read_text())
        self.before = {p.name: p.read_bytes() for p in self.installed.iterdir()}
        self.bin = self.work / "bin"
        self.bin.mkdir()
        for name, body in {
            "curl": 'cat "$BUNDLE_JSON"\n',
            "systemctl": 'echo "$*" >> "$ACTIVATION_LOG"\n[ "$1" != is-active ]\n',
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\n" + body)
            path.chmod(0o755)
        self.activation_log = self.work / "activation.log"

    def run_sync(self, name, key=None):
        chain = (self.root / (name + ".pem")).read_text()
        if name != "untrusted":
            chain += (self.root / "ca.pem").read_text()
        bundle = (self.root / ((key or name) + ".key")).read_text() + chain
        bundle_json = self.work / "bundle.json"
        bundle_json.write_text(json.dumps({"value": bundle}))
        env = {
            **os.environ,
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "BUNDLE_JSON": str(bundle_json),
            "ACTIVATION_LOG": str(self.activation_log),
            "SSL_CERT_FILE": str(self.root / "ca.pem"),
            "SSL_CERT_DIR": str(self.work / "no-system-certificates"),
        }
        return subprocess.run(
            ["bash", "-c",
             'source "$SCRIPT"; DIR="$INSTALLED"; kv_token() { echo fake-token; }; sync_certificate'],
            env={**env, "SCRIPT": str(SCRIPT.resolve()), "INSTALLED": str(self.installed)},
            text=True, capture_output=True, timeout=20,
        )

    def test_rejected_bundles_preserve_installed_pair_without_activation(self):
        for name, key, reason in (
            ("wronghost", None, "certificate is not for"),
            ("expiring", None, "certificate expires within"),
            ("valid", "current", "key does not match"),
            ("untrusted", None, "chain does not verify"),
            ("rollback", None, "older than the installed"),
        ):
            with self.subTest(bundle=name, key=key):
                result = self.run_sync(name, key)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(reason, result.stderr)
                self.assertEqual(
                    {p.name: p.read_bytes() for p in self.installed.iterdir()}, self.before)
                self.assertFalse(self.activation_log.exists())

    def test_unchanged_valid_bundle_recovers_caddy_without_replacing_files(self):
        result = self.run_sync("current")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            {p.name: p.read_bytes() for p in self.installed.iterdir()}, self.before)
        self.assertIn("restart caddy", self.activation_log.read_text())


if __name__ == "__main__":
    unittest.main()
