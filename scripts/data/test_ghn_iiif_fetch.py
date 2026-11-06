import base64
import email.message
import email.utils
from datetime import datetime, timedelta, timezone
import http.server
import importlib.util
import io
from pathlib import Path
import tempfile
import threading
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


# A real 16x24 grayscale baseline JPEG (Pillow, quality 50).
JPEG = base64.b64decode(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDABALDA4MChAODQ4SERATGCgaGBYWGDEjJR0oOjM9PDkzODdASFxOQERXRTc4UG1RV19iZ2hn"
    "Pk1xeXBkeFxlZ2P/wAALCAAYABABAREA/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9"
    "AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZn"
    "aGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP0"
    "9fb3+Pn6/9oACAEBAAA/AMfRLT7vFddNdpoujy3jbS6jbEp/ic9BjIz6nHOAazdEtPu8VX8YXvnXkGkxfcgxJLx1cj5R07Kc8H+L2rct"
    "Gh06xlvLg4ihQs3TJ9hnueg9zXG2KveXst1Iqh55GkYKOAScnH51/9k="
)
SOS = JPEG.index(b"\xff\xda")


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
    PAGE = {"id": "sn83020866/1828-02-21/ed-1/seq-1", "width": 16, "height": 24,
            "image_service": SERVICE.format("batch_gua_cph_ver03")}

    def test_jpeg_size_needs_a_complete_jpeg_with_scan_data(self):
        self.assertEqual(fetch.jpeg_size(JPEG), (16, 24))
        self.assertIsNone(fetch.jpeg_size(JPEG[:-2]))
        self.assertIsNone(fetch.jpeg_size(JPEG[:SOS] + b"\xff\xd9"))
        self.assertIsNone(fetch.jpeg_size(JPEG[:SOS + 10] + b"\xff\xd9"))
        self.assertIsNone(fetch.jpeg_size(b"<html></html>"))

    def test_resume_keeps_valid_images_and_replaces_incomplete_ones(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            path = out / "images" / "sn83020866/1828-02-21/ed-1/seq-1.jpg"
            path.parent.mkdir(parents=True)
            path.write_bytes(JPEG)
            client = FakeClient([])
            self.assertFalse(fetch.fetch_image(client, self.PAGE, out)[2])
            self.assertEqual(client.urls, [])

            path.write_bytes(JPEG[:SOS] + b"\xff\xd9")
            client = FakeClient([("image/jpeg", JPEG)])
            self.assertTrue(fetch.fetch_image(client, self.PAGE, out)[2])
            self.assertEqual(client.urls, [self.PAGE["image_service"] + "/full/full/0/default.jpg"])
            self.assertEqual(path.read_bytes(), JPEG)

    def test_an_image_of_the_wrong_size_stops_the_run(self):
        page = dict(self.PAGE, width=40, height=60)
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(RuntimeError):
                fetch.fetch_image(FakeClient([("image/jpeg", JPEG)]), page, Path(d))
            self.assertFalse((Path(d) / "images" / f"{page['id']}.jpg").exists())


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
    @staticmethod
    def opener(*responses):
        calls = list(responses)

        def open_(req, timeout):
            r = calls.pop(0)
            if isinstance(r, Exception):
                raise r
            return r
        return open_

    def test_retry_waits_at_least_as_long_as_retry_after(self):
        sleeps = []
        client = fetch.Client(delay=0, sleep=sleeps.append,
                              opener=self.opener(http_error(429, "45"), Response(b"{}", "application/json")))
        self.assertEqual(client.get("https://x/"), ("application/json", b"{}"))
        self.assertIn(45, sleeps)
        self.assertEqual(client.requests, 2)

    def test_retry_after_reads_http_dates(self):
        now = datetime(2026, 11, 6, 1, 0, tzinfo=timezone.utc)
        self.assertEqual(fetch.retry_after(email.utils.format_datetime(now + timedelta(seconds=120)), now), 120)
        self.assertEqual(fetch.retry_after("7", now), 7)
        self.assertIsNone(fetch.retry_after("soon", now))

    def test_a_wait_over_the_cap_stops_rather_than_retrying_early(self):
        sleeps = []
        client = fetch.Client(delay=0, sleep=sleeps.append, opener=self.opener(http_error(503, "3600")))
        with self.assertRaises(RuntimeError):
            client.get("https://x/")
        self.assertEqual((client.requests, sleeps), (1, []))

    def test_a_404_is_not_retried(self):
        client = fetch.Client(delay=0, sleep=lambda s: None, opener=self.opener(http_error(404)))
        with self.assertRaises(urllib.error.HTTPError):
            client.get("https://x/")
        self.assertEqual(client.requests, 1)

    def test_redirects_are_not_followed(self):
        seen = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append(self.path)
                self.send_response(302)
                self.send_header("Location", "/turnstile-challenge/")
                self.end_headers()

            def log_message(self, *args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            client = fetch.Client(delay=0, sleep=lambda s: None)
            with self.assertRaises(urllib.error.HTTPError) as e:
                client.get(f"http://127.0.0.1:{server.server_port}/lccn/sn83020866.json")
            self.assertEqual(e.exception.code, 302)
            e.exception.close()
            self.assertEqual(seen, ["/lccn/sn83020866.json"])
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
