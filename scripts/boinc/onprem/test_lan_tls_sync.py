"""Exercise the real shell certificate checks with disposable certificates and no network."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
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
            "curl": 'echo fetch >> "$FETCH_LOG"\ncat "$BUNDLE_JSON"\n',
            "systemctl": (
                'echo "$*" >> "$ACTIVATION_LOG"\n'
                'if [ "$1" = restart ] && [ -n "${BLOCK_ACTIVATION:-}" ]; then\n'
                '  touch "$BLOCK_ACTIVATION"\n'
                '  while [ ! -e "$RELEASE_ACTIVATION" ]; do sleep 0.02; done\n'
                'fi\n'
                '[ "$1" != is-active ]\n'
            ),
        }.items():
            path = self.bin / name
            path.write_text("#!/bin/sh\n" + body)
            path.chmod(0o755)
        self.activation_log = self.work / "activation.log"

    def run_sync(self, name, key=None, locked=False):
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
            "IDENTITY_ENDPOINT": "http://localhost:40342/metadata/identity/oauth2/token",
            "FETCH_LOG": str(self.work / "fetch.log"),
            "TEST_LOCK": str(self.work / "sync.lock"),
            "START_LOG": str(self.work / "started.log"),
        }
        if locked:
            env.update(BLOCK_ACTIVATION=str(self.work / "activating"),
                       RELEASE_ACTIVATION=str(self.work / "release"))
        args = [
            "bash", "-c",
            'source "$SCRIPT"; DIR="$INSTALLED"; LOCK_FILE="$TEST_LOCK"; '
            'kv_token() { echo fake-token; }; '
            + ('echo started >> "$START_LOG"; run_locked' if locked else 'sync_certificate'),
        ]
        kwargs = dict(
            env={**env, "SCRIPT": str(SCRIPT.resolve()), "INSTALLED": str(self.installed)},
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        if locked:
            return subprocess.Popen(args, **kwargs)
        return subprocess.run(args, **kwargs, timeout=20)

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

    def test_locked_entry_point_blocks_second_fetch_until_activation_finishes(self):
        processes = []
        release = self.work / "release"

        def wait_for(predicate):
            deadline = time.monotonic() + 5
            while not predicate():
                self.assertLess(time.monotonic(), deadline, "entry point did not reach checkpoint")
                time.sleep(0.02)

        try:
            first = self.run_sync("current", locked=True)
            processes.append(first)
            wait_for(lambda: (self.work / "activating").exists())
            self.assertEqual((self.work / "sync.lock").stat().st_mode & 0o777, 0o600)
            second = self.run_sync("current", locked=True)
            processes.append(second)
            wait_for(lambda: len((self.work / "started.log").read_text().splitlines()) == 2)
            time.sleep(0.2)
            self.assertIsNone(second.poll())
            self.assertEqual((self.work / "fetch.log").read_text().splitlines(), ["fetch"])
            release.touch()
            for process in processes:
                _, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 0, stderr)
            self.assertEqual((self.work / "fetch.log").read_text().splitlines(), ["fetch", "fetch"])
        finally:
            release.touch()
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                process.communicate(timeout=10)

    @unittest.skipUnless(
        sys.platform.startswith("linux") and os.geteuid() == 0,
        "the fixed service lock requires Linux/root; CI runs this with sudo",
    )
    def test_service_executable_waits_for_lock_before_requesting_identity(self):
        import fcntl

        marker = self.work / "identity-request"
        curl = self.bin / "curl"
        curl.write_text('#!/bin/sh\ntouch "$IDENTITY_REQUEST"\n')
        lock_path = Path("/run/usnm-boinc-lan-tls.lock")
        with os.fdopen(os.open(lock_path, os.O_CREAT | os.O_WRONLY, 0o600), "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            process = subprocess.Popen(
                ["bash", str(SCRIPT.resolve()), "--dir", str(self.installed)],
                env={
                    **os.environ,
                    "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "RUNTIME_DIRECTORY": str(self.work / "unrelated-runtime"),
                    "IDENTITY_ENDPOINT": "http://localhost:40342/metadata/identity/oauth2/token",
                    "IDENTITY_REQUEST": str(marker),
                },
                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            try:
                time.sleep(0.2)
                self.assertIsNone(process.poll())
                self.assertFalse(marker.exists())
                fcntl.flock(lock, fcntl.LOCK_UN)
                _, stderr = process.communicate(timeout=10)
                self.assertTrue(marker.exists())
                self.assertNotEqual(process.returncode, 0)
                self.assertIn("no Key Vault token", stderr)
                self.assertEqual(
                    {name: (self.installed / name).read_bytes() for name in self.before},
                    self.before)
            finally:
                if process.poll() is None:
                    process.terminate()
                process.communicate(timeout=10)


if __name__ == "__main__":
    unittest.main()
