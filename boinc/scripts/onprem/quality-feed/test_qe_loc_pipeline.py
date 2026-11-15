"""qe_loc_pipeline.sh with a stand-in for ssh: the pause window, pacing, shipping and the summary line."""
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PIPELINE = HERE / "qe_loc_pipeline.sh"
# 18 Nov 2026 9:00 AM ET is 14:00 UTC.
BEFORE = 1795010000  # 17 Nov 2026
AFTER = 1795100000  # 18 Nov 2026 15:00 UTC

FAKE_SSH = """#!/bin/sh
# Records each call; answers the CT 230 helper from files in $FAKE.
echo "$*" >> "$FAKE/calls"
case "$*" in
  *"--command status"*) cat "$FAKE/status" ;;
  *"--command collect"*) cat "$FAKE/collect" ;;
  *"--command submit"*) n=$(echo "$*" | sed -n 's/.*--count \\([0-9]*\\).*/\\1/p'); echo "$n" > "$FAKE/submit_count"; echo "{\\"submitted\\":$n}" ;;
  *"--command prune"*) echo pruned=0 ;;
  *"tar -x"*) cat > "$FAKE/shipped.tar" ;;
esac
"""


class Pipeline(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = Path(self.tmp.name)
        self.state, self.fake = t / "state", t / "fake"
        (self.state / "units").mkdir(parents=True)
        self.fake.mkdir()
        ssh = t / "ssh"
        ssh.write_text(FAKE_SSH)
        ssh.chmod(ssh.stat().st_mode | stat.S_IXUSR)
        self.ssh = ssh
        self.set_status(inflight=0, staged=0, free=500)
        (self.fake / "collect").write_text(
            '{"units_collected":3,"rows_new":15000,"spot_checked":1,"hosts_failed":[7,9],"units_resent":2}\n'
        )

    def tearDown(self):
        self.tmp.cleanup()

    def set_status(self, inflight, staged, free, need=""):
        (self.fake / "status").write_text(
            f"inflight={inflight}\nstaged={staged}\nfree_gb={free}\nneed={need}\n"
        )

    def unit(self, name):
        d = self.state / "units" / name
        d.mkdir()
        (d / "qe_text.json").write_text("{}")

    def run_pipeline(self, now=BEFORE, *args):
        env = dict(os.environ, QE_LOC_SSH=str(self.ssh), QE_LOC_NOW=str(now), FAKE=str(self.fake))
        r = subprocess.run(
            ["bash", str(PIPELINE), "--state", str(self.state), *args],
            env=env, capture_output=True, text=True,
        )
        return r.stdout + r.stderr

    def calls(self):
        p = self.fake / "calls"
        return p.read_text().splitlines() if p.exists() else []

    def test_it_stops_from_the_rebuild_until_rebuild_done_exists(self):
        out = self.run_pipeline(AFTER)
        self.assertIn("paused for the rebuild", out)
        self.assertEqual(self.calls(), [])
        (self.state / "rebuild-done").write_text("")
        self.run_pipeline(AFTER)
        self.assertTrue(self.calls())
        self.assertEqual(self.run_pipeline(BEFORE).count("paused"), 0)

    def test_submissions_are_paced_by_the_in_flight_cap(self):
        self.set_status(inflight=190, staged=0, free=500)
        self.run_pipeline(BEFORE, "--max-inflight", "200", "--submit-per-run", "60")
        self.assertEqual((self.fake / "submit_count").read_text().strip(), "10")
        (self.fake / "submit_count").unlink()
        self.set_status(inflight=250, staged=0, free=500)
        self.run_pipeline(BEFORE, "--max-inflight", "200")
        self.assertFalse((self.fake / "submit_count").exists())
        (self.fake / "submit_count").unlink(missing_ok=True)
        self.set_status(inflight=0, staged=0, free=500)
        self.run_pipeline(BEFORE, "--max-inflight", "200", "--submit-per-run", "60")
        self.assertEqual((self.fake / "submit_count").read_text().strip(), "60")

    def test_little_free_space_ships_and_submits_nothing(self):
        self.unit("aa")
        self.set_status(inflight=0, staged=0, free=10)
        out = self.run_pipeline(BEFORE, "--min-free-gb", "150")
        self.assertIn("nothing shipped or submitted", out)
        self.assertFalse((self.state / "shipped" / "aa").exists())
        self.assertFalse((self.fake / "submit_count").exists())

    def test_new_units_are_shipped_once_up_to_the_staging_cap(self):
        for n in ("a1", "a2", "a3", "a4"):
            self.unit(n)
        self.set_status(inflight=0, staged=1, free=500)
        out = self.run_pipeline(BEFORE, "--max-staged", "3")
        shipped = sorted(p.name for p in (self.state / "shipped").iterdir())
        self.assertEqual(shipped, ["a1", "a2"])  # 3 allowed, 1 already waiting
        self.assertIn("shipped=2", out)
        self.set_status(inflight=0, staged=3, free=500)
        self.run_pipeline(BEFORE, "--max-staged", "3")
        self.assertEqual(len(list((self.state / "shipped").iterdir())), 2)
        self.set_status(inflight=0, staged=0, free=500)
        self.run_pipeline(BEFORE, "--max-staged", "3")
        self.assertEqual(sorted(p.name for p in (self.state / "shipped").iterdir()), ["a1", "a2", "a3", "a4"])

    def test_the_summary_line_has_every_count(self):
        out = self.run_pipeline(BEFORE)
        line = [l for l in out.splitlines() if "shipped=" in l][-1]
        for key in ("shipped=0", "submitted=60", "collected=3", "rows=15000", "spot_pass=1", "spot_fail=1", "invalid_hosts=2", "resent=2"):
            self.assertIn(key, line)

    def test_a_pruned_unit_a_resend_needs_is_shipped_first(self):
        self.unit("zz")
        self.unit("aa")
        self.set_status(inflight=0, staged=0, free=500, need="zz")
        (self.state / "shipped").mkdir(exist_ok=True)
        (self.state / "shipped" / "zz").write_text("")
        self.run_pipeline(BEFORE, "--max-staged", "1")
        self.assertTrue((self.fake / "shipped.tar").stat().st_size > 0)
        # zz went (again), aa waits.
        self.assertFalse((self.state / "shipped" / "aa").exists())


if __name__ == "__main__":
    unittest.main()
