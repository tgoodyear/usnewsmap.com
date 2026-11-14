"""sync_curated_pages.py: which blob names it accepts and where it will write (#1956)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sync_curated_pages as m  # noqa: E402


class SafeNames(unittest.TestCase):
    def test_accepts_title_and_part(self):
        self.assertTrue(m.safe_rel("2010218519/20261106T051701091514Z-c90a5d97-0000.parquet"))

    def test_rejects_everything_else(self):
        for rel in ["../x.parquet", "a/../b.parquet", "a/./b.parquet", "/a/b.parquet", "a//b.parquet",
                    "a/b/c.parquet", "a/b.txt", "b.parquet", "a\\b.parquet", "a/b\0.parquet"]:
            self.assertFalse(m.safe_rel(rel), rel)

    def test_inside_follows_symlinks(self):
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as other:
            os.symlink(other, os.path.join(root, "link"))
            os.mkdir(os.path.join(root, "real"))
            self.assertTrue(m.inside(root, os.path.join(root, "real")))
            self.assertFalse(m.inside(root, os.path.join(root, "link")))


if __name__ == "__main__":
    unittest.main()
