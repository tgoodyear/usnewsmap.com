"""bin/start's pid directory is on the container's disk, linked into the project tree."""

import getpass
import grp
import os
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("boinc-daemons.sh")
HOST = socket.gethostname().split(".")[0]


def local_pid_dir(proj: Path, root: Path) -> subprocess.CompletedProcess:
    env = dict(os.environ, USNM_PID_ROOT=str(root), USNM_BOINC_USER=getpass.getuser(),
               USNM_BOINC_GROUP=grp.getgrgid(os.getgid()).gr_name)
    return subprocess.run(
        ["bash", "-c", 'set -euo pipefail; source "$1"; proj="$2"; local_pid_dir', "-", SCRIPT, proj],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )


class LocalPidDir(unittest.TestCase):
    def setUp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        self.proj = Path(t.name, "share", "usnmocr")
        self.root = Path(t.name, "local")
        self.proj.mkdir(parents=True)

    def test_links_the_hosts_pid_directory_to_local_disk(self):
        local_pid_dir(self.proj, self.root)
        link = self.proj / f"pid_{HOST}"
        self.assertTrue(link.is_symlink())
        self.assertEqual(Path(os.readlink(link)), self.root / f"pid_{HOST}")
        self.assertTrue(link.is_dir())
        self.assertEqual((self.root / f"pid_{HOST}").stat().st_mode & 0o777, 0o775)

    def test_start_lock_reopens_after_unlink_with_a_handle_open(self):
        # What bin/start does: open the lock, leave it open in a daemon, unlink it, open it again.
        local_pid_dir(self.proj, self.root)
        lock = self.proj / f"pid_{HOST}" / f"start.lock.{HOST}"
        held = open(lock, "w")
        self.addCleanup(held.close)
        os.unlink(lock)
        open(lock, "w").close()

    def test_is_idempotent_and_repairs_a_wrong_link(self):
        link = self.proj / f"pid_{HOST}"
        link.symlink_to(self.root / "elsewhere")
        local_pid_dir(self.proj, self.root)
        local_pid_dir(self.proj, self.root)
        self.assertEqual(Path(os.readlink(link)), self.root / f"pid_{HOST}")

    def test_keeps_an_existing_directory(self):
        existing = self.proj / f"pid_{HOST}"
        existing.mkdir()
        (existing / "feeder.pid").write_text("123\n")
        r = local_pid_dir(self.proj, self.root)
        self.assertFalse(existing.is_symlink())
        self.assertEqual((existing / "feeder.pid").read_text(), "123\n")
        self.assertIn("not a link", r.stderr)


if __name__ == "__main__":
    unittest.main()
