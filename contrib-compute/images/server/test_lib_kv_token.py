"""kv_token's Arc branch refuses endpoints off loopback: python3 -m unittest -v contrib-compute/images/server/test_lib_kv_token.py

The Arc agent's challenge key must never go to a host the environment names. These cases stop
at the endpoint check, before any request.
"""

import os
import subprocess
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parent / "lib.sh"
ENV = {"KEY_VAULT_NAME": "kv", "DB_HOST": "db", "DB_NAME": "usnmocr", "MASTER_HOST": "boinc.example",
       "PROJECT_DIR": "/nonexistent", "IMDS_ENDPOINT": "http://localhost:40342"}


def kv_token(endpoint):
    env = {"PATH": os.environ["PATH"], **ENV, "IDENTITY_ENDPOINT": endpoint}
    return subprocess.run(["bash", "-c", '. "$1"; kv_token', "_", str(LIB)], env=env,
                          capture_output=True, text=True, timeout=30)


class ArcEndpoint(unittest.TestCase):
    def test_off_loopback_endpoints_are_refused(self):
        for endpoint in ("http://evil.example/metadata/identity/oauth2/token",
                         "https://localhost:40342/metadata/identity/oauth2/token",
                         "http://localhost.evil.example/x", "http://localhost:40342@evil.example/x",
                         "http://127.0.0.1.evil.example/x", "http://192.0.2.1:40342/x"):
            with self.subTest(endpoint=endpoint):
                done = kv_token(endpoint)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn("must be http on loopback", done.stderr)


if __name__ == "__main__":
    unittest.main()
