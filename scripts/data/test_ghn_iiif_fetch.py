import email.message
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import urllib.error

spec = importlib.util.spec_from_file_location(
    "ghn_iiif_fetch", Path(__file__).with_name("ghn-iiif-fetch.py")
)
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)

SERVICE = "https://iiif-ha.galib.uga.edu/iiif/2/newspapers/{}%2Fdata%2Fx%2F0001.jp2"


def canvas(batch, seq, width=4801, height=7456):
    return {"images": [{
        "resource": {"service": {"@id": SERVICE.format(batch)}, "width": width, "height": height},
        "rendering": [
            {"@id": f"https://gahistoricnewspapers.galileo.usg.edu/lccn/sn83020866/1828-02-21/ed-1/seq-{seq}.pdf",
             "format": "application/pdf"},
            {"@id": f"https://gahistoricnewspapers.galileo.usg.edu/lccn/sn83020866/1828-02-21/ed-1/seq-{seq}/",
             "format": "text/html"},
        ],
    }]}


def jpeg(width, height, complete=True):
    sof = b"\xff\xc0\x00\x0b\x08" + height.to_bytes(2, "big") + width.to_bytes(2, "big") + b"\x01\x01\x11\x00"
    data = b"\xff\xd8" + b"\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00" + sof + b"\x00" * 8
    return data + (b"\xff\xd9" if complete else b"")


class FakeClient:
    def __init__(self, responses):
        self.responses, self.urls = list(responses), []

    def get(self, url):
        self.urls.append(url)
        return self.responses.pop(0)


class ManifestTests(unittest.TestCase):
    def test_loc_batch_pages_are_told_apart_from_ghn_scans(self):
        manifest = {"sequences": [{"canvases": [
            canvas("batch_dlc_misctopsn83020866_ver01", 1),
            canvas("batch_gua_cph_ver03", 2, 4795, 7288),
        ]}]}
        got = fetch.pages(manifest)
        self.assertEqual([p["id"] for p in got], ["sn83020866/1828-02-21/ed-1/seq-1", "sn83020866/1828-02-21/ed-1/seq-2"])
        self.assertEqual([p["source"] for p in got], ["loc", "ghn"])
        self.assertEqual((got[1]["batch"], got[1]["width"], got[1]["height"]), ("batch_gua_cph_ver03", 4795, 7288))

    def test_image_services_off_the_known_iiif_host_are_refused(self):
        c = canvas("batch_gua_cph_ver03", 1)
        c["images"][0]["resource"]["service"]["@id"] = "https://example.org/iiif/x.jp2"
        with self.assertRaises(ValueError):
            fetch.pages({"sequences": [{"canvases": [c]}]})

    def test_a_challenge_page_stops_the_run_and_is_not_cached(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "manifests" / "sn83020866.json"
            client = FakeClient([("text/html; charset=utf-8", b"<html>Just a moment...</html>")])
            with self.assertRaises(RuntimeError):
                fetch.cached_json(client, "https://gahistoricnewspapers.galileo.usg.edu/lccn/sn83020866.json", path)
            self.assertFalse(path.exists())


class ImageTests(unittest.TestCase):
    def test_jpeg_size_needs_a_complete_jpeg(self):
        self.assertEqual(fetch.jpeg_size(jpeg(4801, 7456)), (4801, 7456))
        self.assertIsNone(fetch.jpeg_size(jpeg(4801, 7456, complete=False)))
        self.assertIsNone(fetch.jpeg_size(b"<html></html>"))

    def test_resume_keeps_valid_images_and_replaces_wrong_ones(self):
        page = {"id": "sn83020866/1828-02-21/ed-1/seq-1", "width": 40, "height": 60,
                "image_service": SERVICE.format("batch_gua_cph_ver03")}
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            path = out / "images" / "sn83020866/1828-02-21/ed-1/seq-1.jpg"
            path.parent.mkdir(parents=True)
            path.write_bytes(jpeg(40, 60))
            client = FakeClient([])
            self.assertFalse(fetch.fetch_image(client, page, out)[2])
            self.assertEqual(client.urls, [])

            path.write_bytes(jpeg(40, 60, complete=False))
            client = FakeClient([("image/jpeg", jpeg(40, 60))])
            self.assertTrue(fetch.fetch_image(client, page, out)[2])
            self.assertEqual(client.urls, [page["image_service"] + "/full/full/0/default.jpg"])
            self.assertEqual(fetch.jpeg_size(path.read_bytes()), (40, 60))

    def test_an_image_of_the_wrong_size_stops_the_run(self):
        page = {"id": "sn83020866/1828-02-21/ed-1/seq-1", "width": 40, "height": 60,
                "image_service": SERVICE.format("batch_gua_cph_ver03")}
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(RuntimeError):
                fetch.fetch_image(FakeClient([("image/jpeg", jpeg(20, 30))]), page, Path(d))
            self.assertFalse((Path(d) / "images" / "sn83020866/1828-02-21/ed-1/seq-1.jpg").exists())


class Response(io.BytesIO):
    def __init__(self, body, ctype):
        super().__init__(body)
        self.headers = {"Content-Type": ctype}


def http_error(code, retry_after=None):
    headers = email.message.Message()
    if retry_after is not None:
        headers["Retry-After"] = retry_after
    return urllib.error.HTTPError("https://x/", code, "error", headers, None)


class ClientTests(unittest.TestCase):
    def test_429_waits_for_retry_after_then_succeeds(self):
        calls, sleeps = [http_error(429, "7"), Response(b"{}", "application/json")], []

        def opener(req, timeout):
            r = calls.pop(0)
            if isinstance(r, Exception):
                raise r
            return r

        client = fetch.Client(delay=0, sleep=sleeps.append, opener=opener)
        self.assertEqual(client.get("https://x/"), ("application/json", b"{}"))
        self.assertIn(7, sleeps)
        self.assertEqual(client.requests, 2)

    def test_a_404_is_not_retried(self):
        def opener(req, timeout):
            raise http_error(404)

        client = fetch.Client(delay=0, sleep=lambda s: None, opener=opener)
        with self.assertRaises(urllib.error.HTTPError):
            client.get("https://x/")
        self.assertEqual(client.requests, 1)


if __name__ == "__main__":
    unittest.main()
