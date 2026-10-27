"""Tests for quality-jpeg-cache.py (#850): python3 -m unittest -v scripts/ocr-node/test_quality_jpeg_cache.py"""

import hashlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.error
from email.message import Message
from unittest import mock

import PIL
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("qjc", os.path.join(HERE, "quality-jpeg-cache.py"))
qjc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qjc)


def encode(img, fmt, **kw):
    b = io.BytesIO()
    img.save(b, fmt, **kw)
    return b.getvalue()


def http_error(code, headers=None):
    m = Message()
    for k, v in (headers or {}).items():
        m[k] = v
    return urllib.error.HTTPError("http://x", code, "err", m, io.BytesIO(b""))


class Resp:
    def __init__(self, body=b"", headers=None):
        self.body, self.headers = body, headers or {}

    def read(self):
        return self.body


class Recipe(unittest.TestCase):
    def test_feeder_and_cache_make_identical_page_jpegs(self):
        path = os.path.join(HERE, "../../boinc/scripts/onprem/quality-feed/quality_feed.py")
        spec = importlib.util.spec_from_file_location("quality_feed", path)
        feed = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(feed)
        for fmt, size, mode in (("JPEG2000", (4100, 60), "RGB"),
                                ("JPEG2000", (300, 200), "L"),
                                ("JPEG", (100, 80), "RGB"),
                                ("JPEG", (100, 80), "L")):
            with self.subTest(fmt=fmt, size=size, mode=mode):
                pixels = bytes((i * 37) % 256 for i in range(size[0] * size[1] * len(mode)))
                img = Image.frombytes(mode, size, pixels)
                raw = encode(img, fmt)
                data, dimensions = feed.page_jpeg(raw, fmt == "JPEG2000")
                cached = qjc.convert(raw, fmt == "JPEG2000")
                if cached is None:
                    self.assertEqual(data, raw)
                    self.assertEqual(dimensions, size)
                else:
                    self.assertEqual((data, *dimensions), cached)

    def test_jp2_is_grayscale_q75(self):
        raw = encode(Image.new("RGB", (300, 200), (200, 100, 50)), "JPEG2000")
        data, w, h = qjc.convert(raw, True)
        with Image.open(io.BytesIO(data)) as im:
            self.assertEqual((im.format, im.mode, im.size), ("JPEG", "L", (300, 200)))
        self.assertEqual((w, h), (300, 200))
        expect = encode(Image.open(io.BytesIO(raw)).convert("L"), "JPEG", quality=75)
        self.assertEqual(data, expect)

    def test_large_jp2_is_halved(self):
        raw = encode(Image.new("L", (4100, 60)), "JPEG2000")
        _, w, h = qjc.convert(raw, True)
        self.assertEqual((w, h), (2050, 30))

    def test_gray_jpeg_goes_as_is(self):
        raw = encode(Image.new("L", (100, 80)), "JPEG")
        self.assertIsNone(qjc.convert(raw, False))

    def test_color_jpeg_is_converted(self):
        raw = encode(Image.new("RGB", (100, 80)), "JPEG")
        data, w, h = qjc.convert(raw, False)
        self.assertEqual((w, h), (100, 80))
        with Image.open(io.BytesIO(data)) as im:
            self.assertEqual(im.mode, "L")


class Names(unittest.TestCase):
    def test_kept_scans_pass(self):
        n = "keep/batch_az_a_ver01/sn123/1900-01-01/ed-1/seq-1.jp2"
        self.assertEqual(qjc.validate_blob_name(n), n)

    def test_bad_names_are_refused(self):
        for n in ["/keep/a.jp2", "keep/../x.jp2", "keep/a b.jp2", "other/a.jp2", "keep/a.png", "keep//a.jp2"]:
            with self.subTest(n=n), self.assertRaises(qjc.Refused):
                qjc.validate_blob_name(n)


class Masters(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        os.makedirs(f"{self.root}/keep/b")
        self.raw = encode(Image.new("L", (64, 48), 9), "JPEG2000")
        self.path = f"{self.root}/keep/b/1.jp2"
        with open(self.path, "wb") as f:
            f.write(self.raw)
        self.sidecar(hashlib.sha256(self.raw).hexdigest())

    def sidecar(self, sha):
        with open(f"{self.path}.json", "w") as f:
            json.dump({"sha256": sha}, f)

    def test_matching_sidecar(self):
        raw, sha = qjc.read_master(self.root, "keep/b/1.jp2")
        self.assertEqual(raw, self.raw)

    def test_mismatch_is_skipped_not_fatal(self):
        self.sidecar("0" * 64)
        name, outcome, _ = qjc.process(self.root, "keep/b/1.jp2", blobs=mock.Mock())
        self.assertTrue(outcome.startswith("skipped:"), outcome)

    def test_symlink_is_refused(self):
        os.symlink(self.path, f"{self.root}/keep/b/2.jp2")
        with self.assertRaises(qjc.Refused):
            qjc.read_master(self.root, "keep/b/2.jp2")

    def test_upload_carries_provenance(self):
        blobs = mock.Mock()
        blobs.put_once.return_value = True
        _, outcome, _ = qjc.process(self.root, "keep/b/1.jp2", blobs=blobs)
        self.assertEqual(outcome, "uploaded")
        blob, data, meta = blobs.put_once.call_args.args
        self.assertEqual(blob, "quality-jpeg/l-q75-r2/keep/b/1.jp2.jpg")
        self.assertEqual(meta["source_sha256"], hashlib.sha256(self.raw).hexdigest())
        self.assertEqual(meta["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual((meta["width"], meta["height"], meta["recipe"]), (64, 48, "l-q75-r2"))

    def test_storage_refusal_is_picklable(self):
        import pickle
        blobs = mock.Mock()
        blobs.put_once.side_effect = http_error(403, {"x-ms-error-code": "AuthorizationPermissionMismatch"})
        with self.assertRaises(qjc.Refused) as cm:
            qjc.process(self.root, "keep/b/1.jp2", blobs=blobs)
        self.assertIn("AuthorizationPermissionMismatch", str(cm.exception))
        # The script runs as __main__ in both processes; here it needs a module name.
        with mock.patch.dict(sys.modules, {"qjc": qjc}):
            pickle.loads(pickle.dumps(cm.exception))

    def test_main_resumes_from_ledger(self):
        lst, state = f"{self.root}/list.txt", f"{self.root}/state"
        with open(lst, "w") as f:
            f.write("keep/b/1.jp2\n")
        with mock.patch.object(qjc, "PILLOW_VERSION", PIL.__version__), \
                mock.patch.object(qjc, "ProcessPoolExecutor", FakePool), \
                mock.patch.object(qjc.Blobs, "put_once", return_value=True) as put:
            qjc.main(["--account", "acct123", "--list", lst, "--scans-root", self.root, "--state", state])
            qjc.main(["--account", "acct123", "--list", lst, "--scans-root", self.root, "--state", state])
        self.assertEqual(put.call_count, 1)
        with open(f"{state}/done-{qjc.RECIPE}.txt") as f:
            self.assertEqual(f.read(), "keep/b/1.jp2\tuploaded\n")

    def test_new_recipe_does_not_skip_pages_in_old_ledgers(self):
        lst, state = f"{self.root}/list.txt", f"{self.root}/state"
        with open(lst, "w") as f:
            f.write("keep/b/1.jp2\n")
        os.makedirs(state)
        for ledger in ("done.txt", "done-l-q75-r1.txt"):
            with open(f"{state}/{ledger}", "w") as f:
                f.write("keep/b/1.jp2\tuploaded\n")
        with mock.patch.object(qjc, "ProcessPoolExecutor", FakePool), \
                mock.patch.object(qjc.Blobs, "put_once", return_value=True) as put:
            qjc.main(["--account", "acct123", "--list", lst, "--scans-root", self.root, "--state", state])
        self.assertEqual(put.call_count, 1)
        self.assertTrue(put.call_args.args[0].startswith("quality-jpeg/l-q75-r2/"))

    def test_existing_blob_conflict_does_not_mark_page_done(self):
        lst, state = f"{self.root}/list.txt", f"{self.root}/state"
        with open(lst, "w") as f:
            f.write("keep/b/1.jp2\n")

        def opener(req, timeout):
            if req.get_method() == "PUT":
                raise http_error(412)
            return Resp(headers={"Content-Length": str(len(qjc.convert(self.raw, True)[0]))})

        blobs = qjc.Blobs("https://a.blob.core.windows.net/boinc-handoff", mock.Mock(), opener)
        with mock.patch.object(qjc, "PILLOW_VERSION", PIL.__version__), \
                mock.patch.object(qjc, "ProcessPoolExecutor", FakePool), \
                mock.patch.object(qjc, "_init"), mock.patch.object(qjc, "_blobs", blobs):
            with self.assertRaisesRegex(qjc.Refused, "sha256"):
                qjc.main(["--account", "acct123", "--list", lst, "--scans-root", self.root, "--state", state])
        with open(f"{state}/done-{qjc.RECIPE}.txt") as f:
            self.assertEqual(f.read(), "")


class ScanServer(unittest.TestCase):
    def setUp(self):
        self.raw = encode(Image.new("L", (64, 48), 9), "JPEG2000")
        self.files = {"keep/b/1.jp2": self.raw,
                      "keep/b/1.jp2.json": json.dumps({"sha256": hashlib.sha256(self.raw).hexdigest()}).encode()}
        self.urls = []

    def opener(self, req, timeout):
        self.urls.append(req.full_url)
        rel = req.full_url.split("/scans/", 1)[1]
        if rel not in self.files:
            raise http_error(404)
        return Resp(self.files[rel])

    def test_reads_master_and_sidecar(self):
        raw, sha = qjc.read_master("http://scans.example:8380/", "keep/b/1.jp2", opener=self.opener)
        self.assertEqual(raw, self.raw)
        self.assertEqual(self.urls, ["http://scans.example:8380/scans/keep/b/1.jp2.json",
                                     "http://scans.example:8380/scans/keep/b/1.jp2"])

    def test_mismatch_is_refused(self):
        self.files["keep/b/1.jp2"] = self.raw + b"x"
        with self.assertRaises(qjc.Refused):
            qjc.read_master("http://scans.example:8380", "keep/b/1.jp2", opener=self.opener)

    def test_missing_page_is_refused(self):
        with self.assertRaises(qjc.Refused):
            qjc.read_master("http://scans.example:8380", "keep/b/2.jp2", opener=self.opener)

    def test_unreachable_server_skips_the_page(self):
        # read_master binds urlopen at import, so patch the fetch itself: no real I/O.
        with mock.patch.object(qjc, "_get", side_effect=urllib.error.URLError("down")) as get:
            name, outcome, _ = qjc.process("http://scans.example:8380", "keep/b/1.jp2", blobs=mock.Mock())
        get.assert_called_once()
        self.assertTrue(outcome.startswith("skipped:"), outcome)

    def test_scan_server_must_be_a_bare_origin(self):
        with mock.patch.object(qjc, "PILLOW_VERSION", PIL.__version__), self.assertRaises(SystemExit):
            qjc.main(["--account", "acct123", "--list", "x", "--scan-server", "http://h:8380/scans?x"])


class FakePool:
    def __init__(self, n, initializer=None, initargs=()):
        initializer(*initargs)

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def map(self, fn, items, chunksize=1):
        return map(fn, items)


class Upload(unittest.TestCase):
    def blobs(self, opener):
        tok = mock.Mock()
        tok.get.return_value = "T"
        return qjc.Blobs("https://a.blob.core.windows.net/boinc-handoff", tok, opener)

    def test_put_is_conditional(self):
        seen = []

        def opener(req, timeout):
            seen.append(req)
            return Resp()

        self.assertTrue(self.blobs(opener).put_once("quality-jpeg/x.jpg", b"abc", {"recipe": "r"}))
        h = {k.lower(): v for k, v in seen[0].header_items()}
        self.assertEqual((seen[0].get_method(), h["if-none-match"], h["x-ms-meta-recipe"]), ("PUT", "*", "r"))
        self.assertEqual(h["authorization"], "Bearer T")

    def test_matching_provenance_already_there(self):
        meta = {"sha256": hashlib.sha256(b"abc").hexdigest(), "source_sha256": "a" * 64,
                "recipe": qjc.RECIPE, "pillow": qjc.PILLOW_VERSION, "width": 64, "height": 48}

        def opener(req, timeout):
            if req.get_method() == "PUT":
                raise http_error(409)
            return Resp(headers={"Content-Length": "3",
                                 **{f"X-Ms-Meta-{k}": str(v) for k, v in meta.items()}})

        self.assertFalse(self.blobs(opener).put_once("x.jpg", b"abc", meta))

    def test_same_size_with_missing_or_conflicting_provenance_stops(self):
        meta = {"sha256": hashlib.sha256(b"abc").hexdigest(), "source_sha256": "a" * 64,
                "recipe": qjc.RECIPE, "pillow": qjc.PILLOW_VERSION, "width": 64, "height": 48}
        for status in (409, 412):
            for field in meta:
                for value in (None, "different"):
                    with self.subTest(status=status, field=field, value=value):
                        headers = {"Content-Length": "3",
                                   **{f"x-ms-meta-{k}": str(v) for k, v in meta.items()}}
                        if value is None:
                            del headers[f"x-ms-meta-{field}"]
                        else:
                            headers[f"x-ms-meta-{field}"] = value

                        def opener(req, timeout):
                            if req.get_method() == "PUT":
                                raise http_error(status)
                            return Resp(headers=headers)

                        with self.assertRaisesRegex(qjc.Refused, field):
                            self.blobs(opener).put_once("x.jpg", b"abc", meta)

    def test_other_size_stops(self):
        def opener(req, timeout):
            if req.get_method() == "PUT":
                raise http_error(409)
            return Resp(headers={"Content-Length": "4"})

        with self.assertRaises(qjc.Refused):
            self.blobs(opener).put_once("x.jpg", b"abc", {})

    def test_forbidden_is_raised(self):
        def opener(req, timeout):
            raise http_error(403)

        with self.assertRaises(urllib.error.HTTPError):
            self.blobs(opener).put_once("x.jpg", b"abc", {})


class Token(unittest.TestCase):
    def test_arc_challenge(self):
        calls = []

        def opener(req, timeout):
            calls.append(dict(req.header_items()))
            if len(calls) == 1:
                raise http_error(401, {"WWW-Authenticate": "Basic realm=/var/opt/azcmagent/tokens/k.key"})
            return Resp(json.dumps({"access_token": "AT", "expires_on": "9999999999"}).encode())

        t = qjc.ArcToken(opener=opener)
        with mock.patch("builtins.open", mock.mock_open(read_data="SECRET\n")) as o:
            self.assertEqual(t.get(), "AT")
        o.assert_called_once_with("/var/opt/azcmagent/tokens/k.key")
        self.assertEqual(calls[1]["Authorization"], "Basic SECRET")
        self.assertEqual(t.get(), "AT")
        self.assertEqual(len(calls), 2)

    def test_unexpected_keyfile_is_refused(self):
        def opener(req, timeout):
            raise http_error(401, {"WWW-Authenticate": "Basic realm=/etc/shadow"})

        with self.assertRaises(qjc.Refused):
            qjc.ArcToken(opener=opener).get()


if __name__ == "__main__":
    unittest.main()
