import io
import os
import sys
import unittest
import urllib.error

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fetch_jpegs as fj  # noqa: E402


class Clock:
    def __init__(self):
        self.t, self.slept = 0.0, []

    def clock(self):
        return self.t

    def sleep(self, s):
        self.slept.append(s)
        self.t += s


class Resp(io.BytesIO):
    status = 200

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


class Fetch(unittest.TestCase):
    def test_url(self):
        self.assertEqual(fj.url_for("https://x/iiif/service:a:b/"), "https://x/iiif/service:a:b/full/full/0/default.jpg")

    def test_404_gives_up_after_attempts_and_paces(self):
        c = Clock()
        p = fj.Pacer(1.0, c.clock, c.sleep)
        calls = []

        def opener(req, timeout):
            calls.append(req.full_url)
            raise urllib.error.HTTPError(req.full_url, 404, "nf", {}, None)
        status, body, tries = fj.fetch_one("https://x/a.jpg", p, 3, opener)
        self.assertEqual((status, body, tries, len(calls)), (404, None, 3, 3))
        self.assertEqual(c.slept, [1.0, 1.0])

    def test_success_first_try(self):
        p = fj.Pacer(1.0, Clock().clock, lambda s: None)
        status, body, tries = fj.fetch_one("https://x/a.jpg", p, 3, lambda req, timeout: Resp(b"jpg"))
        self.assertEqual((status, body, tries), (200, b"jpg", 1))


if __name__ == "__main__":
    unittest.main()
