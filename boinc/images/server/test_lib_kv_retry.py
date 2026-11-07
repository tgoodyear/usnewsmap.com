"""Key Vault reads retry transient failures (#958): python3 -m unittest -v boinc/images/server/test_lib_kv_retry.py

A fake curl on PATH fails a set number of times, then answers. kv_token is stubbed, so nothing
reaches the network.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parent / "lib.sh"
ENV = {"KEY_VAULT_NAME": "kv", "DB_HOST": "db", "DB_NAME": "usnmocr", "MASTER_HOST": "boinc.example",
       "PROJECT_DIR": "/nonexistent", "KV_BACKOFF": "0"}
# Fails while the counter is below FAILS; then prints a secret, or a 404 when NOT_FOUND is set.
FAKE_CURL = """#!/bin/bash
n=$(cat "$COUNTER" 2>/dev/null || echo 0); echo $((n + 1)) > "$COUNTER"
out=""; code_fmt=""
while [ $# -gt 0 ]; do case $1 in -o) out=$2; shift 2 ;; -w) code_fmt=$2; shift 2 ;; *) shift ;; esac; done
if [ "$n" -lt "$FAILS" ]; then exit 28; fi
if [ -n "${NOT_FOUND:-}" ]; then echo '{"error":{}}' > "$out"; printf 404; exit 0; fi
if [ -n "$out" ]; then echo '{"value":"s3cret"}' > "$out"; printf 200; else echo '{"value":"s3cret"}'; fi
"""


class KvRetry(unittest.TestCase):
    def run_lib(self, script, fails, **extra):
        with tempfile.TemporaryDirectory() as d:
            bin_dir = Path(d, "bin"); bin_dir.mkdir()
            curl = bin_dir / "curl"; curl.write_text(FAKE_CURL); curl.chmod(0o755)
            env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", **ENV, "COUNTER": f"{d}/n",
                   "FAILS": str(fails), "OUT": f"{d}/secret", **extra}
            done = subprocess.run(["bash", "-c", '. "$1"; kv_token() { echo tok; }; ' + script, "_", str(LIB)],
                                  env=env, capture_output=True, text=True, timeout=30)
            calls = int(Path(d, "n").read_text()) if Path(d, "n").exists() else 0
            secret = Path(d, "secret").read_text() if Path(d, "secret").exists() else None
            return done, calls, secret

    def test_kv_get_retries_then_succeeds(self):
        done, calls, secret = self.run_lib('kv_get name "$OUT"', fails=2)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual((calls, secret), (3, "s3cret\n"))
        self.assertIn("try 1 of 3", done.stderr)

    def test_kv_get_gives_up_after_three_tries(self):
        done, calls, _ = self.run_lib('kv_get name "$OUT"', fails=5)
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(calls, 3)
        self.assertIn("cannot read Key Vault secret name", done.stderr)

    def test_kv_get_optional_retries_but_a_404_is_final(self):
        done, calls, secret = self.run_lib('kv_get_optional name "$OUT"', fails=1)
        self.assertEqual((done.returncode, calls, secret), (0, 2, "s3cret\n"), done.stderr)
        done, calls, _ = self.run_lib('kv_get_optional name "$OUT" || echo missing', fails=0, NOT_FOUND="1")
        self.assertEqual((done.returncode, calls), (0, 1), done.stderr)
        self.assertIn("missing", done.stdout)

    def test_the_token_is_not_on_curls_command_line(self):
        body = LIB.read_text().split("kv_get() {")[1].split("\n}\n")[0]
        self.assertNotIn('-H "Authorization: Bearer $token"', body)


if __name__ == "__main__":
    unittest.main()
