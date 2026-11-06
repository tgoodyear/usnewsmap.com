import re
import unittest

import undigitized


class UndigitizedTests(unittest.TestCase):
    def test_the_list_is_the_audits(self):
        self.assertEqual(len(undigitized.PAGES), 24)
        self.assertEqual(len(undigitized.DOC_IDS), 24)
        self.assertEqual(list(undigitized.PAGES), sorted(undigitized.PAGES))
        self.assertRegex(undigitized.RELEASE, r"^data-ja-availability-\d{8}$")
        self.assertRegex(undigitized.CLASSIFICATION_SHA256, r"^[0-9a-f]{64}$")
        for doc_id, metadata in undigitized.PAGES:
            self.assertTrue(re.fullmatch(r"[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+", doc_id), doc_id)
            self.assertTrue(re.fullmatch(r"[0-9a-f]{64}", metadata), doc_id)


if __name__ == "__main__":
    unittest.main()
