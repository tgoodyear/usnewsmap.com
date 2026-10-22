"""Every image that serves Apache and PHP takes Debian security updates and enforces the PHP floor (#969).

The images start from pinned digests, so without the upgrade layer their packages stay at
whatever the pinned image was built with. The overlays start from older live images still.
"""
import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

IMAGES = Path(__file__).resolve().parent.parent
# The serving chain: the server image, the bridge image the on-prem server runs, and the overlays
# built on a pinned live bridge digest. appver (the publish job) and bridge-bin don't serve.
SERVING = [
    IMAGES / "server" / "Containerfile",
    IMAGES / "bridge" / "Containerfile",
    IMAGES / "server" / "live-overlay.Containerfile",
    IMAGES / "server" / "batch-overlay.Containerfile",
    IMAGES / "server" / "amd-885.Containerfile",
    IMAGES / "server" / "security-overlay.Containerfile",
]
# DSA-6255-1: php8.2 8.2.35-1~deb12u1.
MIN_PHP_PATCH = 35


def final_stage(text):
    """The lines after the last FROM, with continuation lines joined."""
    joined = re.sub(r"\\\n", " ", text)
    lines = joined.splitlines()
    last = max(i for i, line in enumerate(lines) if line.startswith("FROM "))
    return lines[last + 1:]


def upgrade_run(text):
    """The RUN instruction in the final stage that upgrades packages."""
    runs = [line for line in final_stage(text) if line.startswith("RUN ") and " upgrade" in line]
    return runs[0][len("RUN "):] if len(runs) == 1 else None


class SecurityUpdates(unittest.TestCase):
    def test_every_server_and_bridge_containerfile_is_covered(self):
        found = set(IMAGES.glob("server/*Containerfile")) | set(IMAGES.glob("bridge/*Containerfile"))
        self.assertEqual(found, set(SERVING))

    def test_final_stage_upgrades_and_checks_the_floor(self):
        for path in SERVING:
            with self.subTest(path=path.relative_to(IMAGES)):
                text = path.read_text()
                run = upgrade_run(text)
                self.assertIsNotNone(run, "no single upgrade RUN in the final stage")
                self.assertIn("apt-get update", run)
                self.assertRegex(run, r"DEBIAN_FRONTEND=noninteractive apt-get -y .*upgrade")
                self.assertIn("rm -rf /var/lib/apt/lists/*", run)
                self.assertIn("dpkg-query -W -f='${Version}' php8.2-common", run)
                self.assertIn('dpkg --compare-versions "$php_version" ge "$PHP_FLOOR"', run)
                self.assertIn("exit 1", run)
                stage = final_stage(text)
                self.assertIn("ARG PHP_FLOOR=" + self.floor(path), stage)
                self.assertTrue(any(line.startswith("ARG SECURITY_REFRESH=") for line in stage))
                # Before anything else in the stage, so later layers are built on patched packages.
                first = next(line for line in stage if line.startswith(("RUN ", "COPY ", "ADD ")))
                self.assertEqual(first[len("RUN "):], run)

    def floor(self, path):
        floors = re.findall(r"^ARG PHP_FLOOR=(\S+)$", path.read_text(), re.M)
        self.assertEqual(len(floors), 1, path)
        return floors[0]

    def test_floors_agree_and_meet_dsa_6255_1(self):
        floors = {self.floor(path) for path in SERVING}
        self.assertEqual(len(floors), 1, floors)
        match = re.fullmatch(r"8\.2\.(\d+)-\S+", floors.pop())
        self.assertIsNotNone(match)
        self.assertGreaterEqual(int(match.group(1)), MIN_PHP_PATCH)

    def run_stubbed(self, path, *, apt_status=0, compare_status=0):
        """Run the upgrade RUN under sh with apt-get, dpkg-query and dpkg stubbed."""
        with tempfile.TemporaryDirectory() as tmp:
            stubs = {
                "apt-get": f"exit {apt_status}",
                "dpkg-query": "echo 8.2.34-1~deb12u1",
                "dpkg": f'[ "$1" = --compare-versions ] && exit {compare_status}; exit 2',
                "rm": "exit 0",
            }
            for name, body in stubs.items():
                stub = Path(tmp, name)
                stub.write_text("#!/bin/sh\n" + body + "\n")
                stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
            env = {"PATH": tmp + os.pathsep + "/usr/bin:/bin", "SECURITY_REFRESH": "x",
                   "PHP_FLOOR": self.floor(path)}
            return subprocess.run(["sh", "-c", upgrade_run(path.read_text())], env=env,
                                  text=True, capture_output=True)

    def test_build_fails_below_the_floor(self):
        for path in SERVING:
            with self.subTest(path=path.relative_to(IMAGES)):
                self.assertEqual(self.run_stubbed(path).returncode, 0)
                below = self.run_stubbed(path, compare_status=1)
                self.assertNotEqual(below.returncode, 0)
                self.assertIn("is older than", below.stderr)
                self.assertNotEqual(self.run_stubbed(path, apt_status=100).returncode, 0)


if __name__ == "__main__":
    unittest.main()
