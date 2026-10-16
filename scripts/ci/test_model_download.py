"""Exercise the actual model-stage download command with a local TLS server."""
import hashlib
import os
from pathlib import Path
import platform
import shlex
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest


ROOT = Path(__file__).resolve().parents[2]
IMAGE = ROOT / "ocr/engines/tesseract/Containerfile"
BODY = b"pinned model fixture\n"


class ModelDownloadTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(self.scratch.cleanup)
        self.directory = Path(self.scratch.name)
        self.cert = self.directory / "cert.pem"
        key = self.directory / "key.pem"
        subprocess.run(
            ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
             "-keyout", str(key), "-out", str(self.cert), "-days", "1",
             "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"],
            check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(self.cert, key)
        text = IMAGE.read_text()
        self.stage = text.split("RUN set -eu;", 1)[1].split("\n\n", 1)[0]
        self.command = ["curl"] + shlex.split(
            self.stage.split("curl ", 1)[1].split(' -o "$name"', 1)[0]
            .replace("\\\n", " ")
        )

    def serve(self, failures):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(0.2)
        stopped = threading.Event()
        attempts = []

        def loop():
            while not stopped.is_set():
                try:
                    connection, _ = listener.accept()
                except socket.timeout:
                    continue
                attempts.append(1)
                with connection:
                    # Closing before ServerHello reproduces curl TLS error 35.
                    if len(attempts) <= failures:
                        continue
                    with self.context.wrap_socket(connection, server_side=True) as tls:
                        tls.recv(8192)
                        tls.sendall(
                            b"HTTP/1.1 200 OK\r\nContent-Length: "
                            + str(len(BODY)).encode()
                            + b"\r\nConnection: close\r\n\r\n" + BODY
                        )

        thread = threading.Thread(target=loop)
        thread.start()

        def cleanup():
            stopped.set()
            thread.join(timeout=3)
            listener.close()

        self.addCleanup(cleanup)
        return f"https://localhost:{listener.getsockname()[1]}/model", attempts

    def test_retry_policy_is_bounded_and_verifies_tls(self):
        args = self.command
        for flag, value in [("--connect-timeout", "20"), ("--max-time", "180"),
                            ("--retry", "3"), ("--retry-max-time", "360")]:
            self.assertEqual(args[args.index(flag) + 1], value)
        self.assertIn("--retry-all-errors", args)
        self.assertNotIn("-k", args)
        self.assertNotIn("--insecure", args)

    def test_real_tls_failure_recovers_and_retains_checksum(self):
        url, attempts = self.serve(failures=1)
        result = self.download(url, hashlib.sha256(BODY).hexdigest())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(attempts), 2)
        self.assertIn("curl: (35)", result.stderr)
        self.assertEqual((self.directory / "models/model").read_bytes(), BODY)

    def test_real_tls_failure_stops_after_three_retries(self):
        url, attempts = self.serve(failures=100)
        result = self.download(url, hashlib.sha256(BODY).hexdigest())
        self.assertEqual(result.returncode, 35, result.stderr)
        self.assertEqual(len(attempts), 4)

    def test_checksum_mismatch_is_not_retried_or_accepted(self):
        url, attempts = self.serve(failures=0)
        result = self.download(url, "0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(attempts), 1)
        self.assertIn("FAILED", result.stdout)

    def download(self, url, digest):
        lock = self.directory / "models.lock"
        lock.write_text(f"model {url} {digest}\n")
        stage = ("set -eu;" + self.stage).replace(
            "/models.lock", "LOCK_FIXTURE"
        ).replace("/models", shlex.quote(str(self.directory / "models")))
        stage = stage.replace("LOCK_FIXTURE", shlex.quote(str(lock)))
        if platform.system() == "Darwin":
            # macOS's sha256sum lacks GNU long options; Linux executes unchanged.
            stage = stage.replace("sha256sum", "shasum -a 256")
        return subprocess.run(
            ["sh", "-c", stage], text=True, capture_output=True, timeout=25,
            env={**os.environ, "CURL_CA_BUNDLE": str(self.cert), "NO_PROXY": "localhost"},
        )


if __name__ == "__main__":
    unittest.main()
