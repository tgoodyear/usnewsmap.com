import importlib.util
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


if __name__ == "__main__":
    unittest.main()
