"""Tests for the Serial Set pilot's scorer fingerprint."""
import importlib.util
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("serialset_pilot", Path(__file__).with_name("serialset-pilot.py"))
pilot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pilot)


class ScorerFingerprintTests(unittest.TestCase):
    def test_tracks_local_edit_distance_source(self):
        dependency = pilot.ROOT / "crates/usnm-ocr-quality"
        paths = set(pilot.score_implementation_paths())
        distance = dependency / "src/distance.rs"
        self.assertIn(dependency / "Cargo.toml", paths)
        self.assertIn(distance, paths)

        original = pilot.score_implementation_sha256()
        read_bytes = Path.read_bytes

        def changed_dependency(path):
            data = read_bytes(path)
            return data + b"changed" if path == distance else data

        with mock.patch.object(Path, "read_bytes", new=changed_dependency):
            self.assertNotEqual(original, pilot.score_implementation_sha256())


if __name__ == "__main__":
    unittest.main()
