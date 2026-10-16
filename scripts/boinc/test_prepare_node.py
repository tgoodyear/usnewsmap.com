"""Offline fail-closed preparation tests; never invoke host account/service tools."""
from datetime import datetime, timedelta, timezone
import hashlib
from pathlib import Path
import tempfile
import unittest

from scripts.boinc import prepare_node as node


class PrepareNodeTests(unittest.TestCase):
    def test_subordinate_ids_are_free_in_both_tables(self):
        self.assertEqual(node.allocation("eng:655360:65536", "other:1048576:65536"), 1114112)
        for value in ("boinc:100000:65536", "eng:-1:65536", "eng:100000:0"):
            with self.assertRaises(ValueError):
                node.allocation(value)

    def test_only_fresh_healthy_same_worker_baseline_passes(self):
        now = datetime.now(timezone.utc)
        baseline = {"observed_at": now.isoformat(), "baseline_ready": True,
                    "worker": {"MainPID": "123"}}
        node.validate_baseline(baseline, "123", now)
        for edits in ({"baseline_ready": False},
                      {"observed_at": (now - timedelta(seconds=121)).isoformat()},
                      {"observed_at": (now + timedelta(seconds=1)).isoformat()},
                      {"worker": {"MainPID": "124"}}):
            with self.assertRaises(ValueError):
                node.validate_baseline({**baseline, **edits}, "123", now)

    def test_source_and_both_actual_binaries_must_verify(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "install/bin").mkdir(parents=True)
            (root / "source-commit").write_text(node.COMMIT)
            lines = []
            for name in ("boinc", "boinccmd"):
                binary = root / "install/bin" / name
                binary.write_bytes(b"fixture client")
                lines.append(hashlib.sha256(binary.read_bytes()).hexdigest() + "  install/bin/" + name)
            (root / "SHA256SUMS").write_text("\n".join(lines))
            self.assertEqual(len(node.validate_bundle(root)), 2)
            (root / "install/bin/boinccmd").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                node.validate_bundle(root)
            (root / "SHA256SUMS").write_text(lines[0])
            with self.assertRaisesRegex(ValueError, "manifest"):
                node.validate_bundle(root)
            (root / "source-commit").write_text("wrong")
            with self.assertRaisesRegex(ValueError, "source"):
                node.validate_bundle(root)

    def test_unit_requires_reviewed_profile_and_never_enables_itself(self):
        self.assertIn("User=boinc\nGroup=boinc", node.UNIT)
        self.assertIn("ConditionPathExists=/etc/systemd/system/" + node.SERVICE + ".d/", node.UNIT)
        self.assertIn("--gui_rpc_port 31417", node.UNIT)
        self.assertIn("Restart=no", node.UNIT)
        self.assertNotIn("usnmeng", node.UNIT)


if __name__ == "__main__":
    unittest.main()
