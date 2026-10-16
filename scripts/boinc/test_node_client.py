"""Offline orchestration tests for the pinned source builder, not a real RHEL build."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("build-node-client.sh")
COMMIT = "ae68f3290ac82d08b4d9767acfe0daaccaeeb883"


class NodeClientBuild(unittest.TestCase):
    def test_build_is_pinned_and_never_installs_host_services(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binaries = root / "bin"
            binaries.mkdir()
            out = root / "build"
            log = root / "calls"
            stub = """#!/usr/bin/env bash
set -eu
echo "$(basename "$0") $*" >> "$CALLS"
case "$(basename "$0")" in
  git)
    case "$*" in
      "init -q "*) mkdir -p "$OUT/source"
        printf '#!/bin/sh\\nexit 0\\n' > "$OUT/source/_autosetup"
        cp "$OUT/source/_autosetup" "$OUT/source/configure"
        chmod +x "$OUT/source/_autosetup" "$OUT/source/configure" ;;
      *"rev-parse HEAD") echo "$COMMIT" ;;
    esac ;;
  make)
    case "$*" in
      *"client install")
        mkdir -p "$OUT/install/bin"
        printf '#!/bin/sh\\necho 8.2.13\\n' > "$OUT/install/bin/boinc"
        cp "$OUT/install/bin/boinc" "$OUT/install/bin/boinccmd"
        chmod +x "$OUT/install/bin/"* ;;
    esac ;;
esac
"""
            for name in ["git", "make", "autoconf", "automake", "libtoolize", "pkg-config", "c++"]:
                path = binaries / name
                path.write_text(stub)
                path.chmod(0o755)
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                       OUT=str(out), CALLS=str(log), COMMIT=COMMIT)
            result = subprocess.run(["bash", str(SCRIPT), "--out", str(out)],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((out / "source-commit").read_text().strip(), COMMIT)
            calls = log.read_text()
            self.assertIn(f"fetch -q --depth 1 https://github.com/BOINC/boinc.git {COMMIT}", calls)
            self.assertIn("make -j 1 -C client install", calls)
            self.assertTrue((out / "SHA256SUMS").exists())
            self.assertNotIn("systemctl", calls)
            repeated = subprocess.run(["bash", str(SCRIPT), "--out", str(out)],
                                      env=env, capture_output=True, text=True)
            self.assertNotEqual(repeated.returncode, 0)
            self.assertIn("refusing to overwrite", repeated.stderr)
            bad_out = root / "bad"
            wrong = subprocess.run(["bash", str(SCRIPT), "--out", str(bad_out)],
                                   env=dict(env, OUT=str(bad_out), COMMIT="wrong"),
                                   capture_output=True, text=True)
            self.assertNotEqual(wrong.returncode, 0)
            self.assertIn("source revision differs", wrong.stderr)


if __name__ == "__main__":
    unittest.main()
