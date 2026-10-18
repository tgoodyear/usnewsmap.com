"""Tests for the checksum-locked EffOCR benchmark adapter."""

import unittest

from effocr_trial import tree_digest


class EffOCRTrialTests(unittest.TestCase):
    def test_tree_digest_is_independent_of_input_order(self):
        first = [
            {"path": "b.onnx", "sha256": "b" * 64},
            {"path": "a.onnx", "sha256": "a" * 64},
        ]
        second = list(reversed(first))
        self.assertEqual(tree_digest(first), tree_digest(second))

    def test_tree_digest_binds_paths_and_file_hashes(self):
        first = [{"path": "a.onnx", "sha256": "a" * 64}]
        changed = [{"path": "b.onnx", "sha256": "a" * 64}]
        self.assertNotEqual(tree_digest(first), tree_digest(changed))


if __name__ == "__main__":
    unittest.main()
