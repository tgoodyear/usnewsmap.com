"""iris-bench: distinct-crop loading, without OpenVINO or a GPU."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_iris_worker import jpeg, job_for  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "boinc/apps/iris"))
import bench  # noqa: E402
import contract as c  # noqa: E402


def unit(root: Path, name: str, page: bytes, crop_id: str) -> None:
    directory = root / name
    directory.mkdir()
    (directory / c.JOB_FILE).write_bytes(c.encode_job(job_for(page, crop_id=crop_id)))
    (directory / c.PAGE_FILE).write_bytes(page)


class LoadUnitsTest(unittest.TestCase):
    def test_distinct_units_and_offset(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for index in range(3):
                unit(root, f"u{index}", jpeg(825, 429, pad=index), f"overproof-ca-000{index}")
            self.assertEqual(len(bench.load_units(root, 3)), 3)
            self.assertEqual(len(bench.load_units(root, 2, offset=1)), 2)
            with self.assertRaises(SystemExit):
                bench.load_units(root, 2, offset=2)

    def test_duplicate_image_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            page = jpeg(825, 429)
            unit(root, "a", page, "overproof-ca-0001")
            unit(root, "b", page, "overproof-ca-0002")
            with self.assertRaises(SystemExit):
                bench.load_units(root, 2)


if __name__ == "__main__":
    unittest.main()
