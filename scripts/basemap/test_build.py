import importlib.util
import io
from concurrent.futures import ThreadPoolExecutor
import time
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("basemap_build", Path(__file__).with_name("build.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def varint(value):
    out = bytearray()
    while value > 127:
        out.append((value & 127) | 128)
        value >>= 7
    return bytes(out + bytes([value]))


def field(number, data):
    return varint(number * 8 + 2) + varint(len(data)) + data


class GlyphTests(unittest.TestCase):
    def test_only_displayed_label_properties_determine_font_ranges(self):
        keys = ["name", "name:ja", "ref"]
        values = ["Chicago", "\u65e5\u672c", "\u00c9"]
        tags = b"".join(varint(n) for n in [0, 0, 1, 1, 2, 2])
        layer = field(2, field(2, tags))
        layer += b"".join(field(3, key.encode()) for key in keys)
        layer += b"".join(field(4, field(1, value.encode())) for value in values)
        self.assertEqual(module.glyph_ranges(field(3, layer)), {0})

    def test_nonlatin_labels_include_their_bmp_ranges_but_not_unsupported_astral_ranges(self):
        layer = field(2, field(2, b"\x00\x00"))
        layer += field(3, b"name:nonlatin")
        layer += field(4, field(1, "\u65e5\U00010400".encode()))
        self.assertEqual(module.glyph_ranges(field(3, layer)), {0, 25856})

    def test_malformed_protobuf_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "Truncated protobuf"):
            list(module.fields(b"\x1a\x02\x01"))
        with self.assertRaisesRegex(ValueError, "Truncated packed"):
            list(module.varints(b"\x80"))


class BudgetTests(unittest.TestCase):
    def test_limit_is_enforced_before_reading_excess_response_bytes(self):
        response = io.BytesIO(b"abcdefghijk")
        budget = module.DownloadBudget(limit=10)
        with self.assertRaisesRegex(ValueError, "offline limit"):
            budget.read(response)
        self.assertEqual(response.tell(), 10)
        self.assertEqual(budget.total, 10)
        with self.assertRaisesRegex(ValueError, "offline limit"):
            budget.check()

    def test_cached_and_network_bytes_share_one_budget(self):
        budget = module.DownloadBudget(limit=10)
        budget.reserve(6)
        response = io.BytesIO(b"abcde")
        with self.assertRaises(ValueError):
            budget.read(response)
        self.assertEqual(response.tell(), 4)
        self.assertEqual(budget.total, 10)

    def test_short_reads_return_unused_reservations(self):
        budget = module.DownloadBudget(limit=100000)
        self.assertEqual(budget.read(io.BytesIO(b"abc")), b"abc")
        self.assertEqual(budget.read(io.BytesIO(b"de")), b"de")
        self.assertEqual(budget.total, 5)

    def test_concurrent_reservations_cannot_overshoot_the_shared_limit(self):
        class SlowStream(io.BytesIO):
            def read(self, size):
                time.sleep(0.002)
                return super().read(size)
        streams = [SlowStream(b"a" * 3000) for _ in range(4)]
        budget = module.DownloadBudget(limit=5000)
        with ThreadPoolExecutor(max_workers=4) as pool:
            futures = [pool.submit(budget.read, stream) for stream in streams]
            failures = 0
            for future in futures:
                try:
                    future.result()
                except ValueError:
                    failures += 1
        self.assertGreater(failures, 0)
        self.assertLessEqual(sum(stream.tell() for stream in streams), 5000)
        self.assertEqual(budget.total, 5000)

if __name__ == "__main__":
    unittest.main()
