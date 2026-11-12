"""Offline tests for chr_advocate_oai.py. Run: python3 -m unittest -v docs/reports/2026-11-cherokee/scripts/test_chr_advocate_oai.py"""
import json, os, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chr_advocate_oai as m

NS = ('xmlns="http://www.openarchives.org/OAI/2.0/" xmlns:oai_dc="http://www.openarchives.org/OAI/2.0/oai_dc/" '
      'xmlns:dc="http://purl.org/dc/elements/1.1/"')


def envelope(verb, inner):
    return (f'<?xml version="1.0"?><OAI-PMH {NS}><responseDate>2026-11-12T00:00:00Z</responseDate>'
            f'<request verb="{verb}">x</request>{inner}</OAI-PMH>').encode()


def rec(ident, date, desc, status=""):
    if status:
        return f'<record><header status="{status}"><identifier>{ident}</identifier></header></record>'
    return (f'<record><header><identifier>{ident}</identifier><setSpec>ADV</setSpec></header><metadata>'
            f'<oai_dc:dc><dc:title>Cherokee Advocate (Tahlequah), {date}</dc:title><dc:date>{date}</dc:date>'
            f'<dc:format>text</dc:format><dc:description>{desc}</dc:description></oai_dc:dc></metadata></record>')


SETS = envelope("ListSets", '<ListSets><set><setSpec>ODNP</setSpec><setName>Oklahoma Digital Newspaper Program</setName></set>'
                '<set><setSpec>ADV</setSpec><setName>Cherokee Advocate</setName></set></ListSets>')
PAGE1 = envelope("ListRecords", '<ListRecords>' + rec("oai:x:1", "1844-09-26", "four pages : ill. ; page 51 x 66 cm")
                 + rec("oai:x:2", "1844-10-03", "Digitized from microfilm") + rec("oai:x:3", "", "gone", "deleted")
                 + '<resumptionToken cursor="0" completeListSize="4">tok1</resumptionToken></ListRecords>')
PAGE2 = envelope("ListRecords", '<ListRecords>' + rec("oai:x:4", "1850-01-04", "8 pp.")
                 + rec("oai:x:5", "1850-01-11", "one page : ill.") + '<resumptionToken/></ListRecords>')


class Fake:
    def __init__(self, bodies):
        self.bodies, self.urls = list(bodies), []
    def __call__(self, url):
        self.urls.append(url)
        b = self.bodies.pop(0)
        if isinstance(b, Exception):
            raise b
        return b


class Clock:
    def __init__(self): self.t, self.sleeps = 100.0, []
    def now(self): return self.t
    def sleep(self, s): self.sleeps.append(s); self.t += s


def harvester(bodies, **kw):
    c = Clock()
    return m.Harvester("https://example.test/oai/", kw.pop("delay", 3.0), kw.pop("max_requests", 50),
                       fetch=Fake(bodies), sleep=c.sleep, clock=c.now, **kw), c


class Tests(unittest.TestCase):
    def test_build_url_token_is_exclusive(self):
        u = m.build_url("https://e/oai/", "ListRecords", "tok1", set="ADV", metadataPrefix="oai_dc")
        self.assertEqual(u, "https://e/oai/?verb=ListRecords&resumptionToken=tok1")
        self.assertIn("set=ADV", m.build_url("https://e/oai/", "ListRecords", None, set="ADV", metadataPrefix="oai_dc"))

    def test_parse_records(self):
        items, tok = m.parse_page(PAGE1, "ListRecords")
        self.assertEqual(tok, "tok1")
        self.assertEqual([(r["year"], r["pages"], r["deleted"]) for r in items],
                         [(1844, 4, False), (1844, None, False), (None, None, True)])

    def test_empty_token_ends_list(self):
        self.assertIsNone(m.parse_page(PAGE2, "ListRecords")[1])

    def test_oai_error(self):
        with self.assertRaises(m.HarvestError):
            m.parse_page(envelope("ListRecords", '<error code="badArgument">bad</error>'), "ListRecords")

    def test_resumption_and_counts(self):
        h, c = harvester([PAGE1, PAGE2])
        res = m.summarize(h.list_records("ADV", "oai_dc"))
        self.assertEqual(h.requests, 2)
        self.assertIn("resumptionToken=tok1", h.fetch.urls[1])
        self.assertEqual((res["issues"], res["pages"], res["issues_with_page_count"], res["deleted_records_skipped"]),
                         (4, 13, 3, 1))
        self.assertEqual((res["year_first"], res["year_last"]), (1844, 1850))
        self.assertEqual(res["years"], {"1844": {"issues": 2, "pages": 4}, "1850": {"issues": 2, "pages": 9}})

    def test_pacing(self):
        h, c = harvester([SETS, PAGE2])
        h.list_sets(); h.list_records("ADV", "oai_dc")
        self.assertEqual(len(c.sleeps), 1)
        self.assertGreaterEqual(c.sleeps[0], 3.0)

    def test_delay_floor(self):
        for bad in (2.0, float("nan"), -1.0):
            with self.assertRaises(m.HarvestError):
                harvester([], delay=bad)

    def test_http_error_stops_without_retry(self):
        h, _ = harvester([PAGE1, m.HarvestError("HTTP 429 for u"), PAGE2])
        with self.assertRaises(m.HarvestError):
            h.list_records("ADV", "oai_dc")
        self.assertEqual(h.requests, 2)
        self.assertEqual(len(h.fetch.bodies), 1)

    def test_redirect_is_an_error(self):
        import http.server, threading
        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(302); self.send_header("Location", "/elsewhere"); self.end_headers()
            def log_message(self, *a): pass
        srv = http.server.HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.handle_request, daemon=True).start()
        try:
            with self.assertRaises(m.HarvestError) as cm:
                m.fetch_url(f"http://127.0.0.1:{srv.server_port}/oai/", timeout=5)
            self.assertIn("HTTP 302", str(cm.exception))
        finally:
            srv.server_close()

    def test_max_requests(self):
        h, _ = harvester([PAGE1, PAGE2], max_requests=1)
        with self.assertRaises(m.HarvestError):
            h.list_records("ADV", "oai_dc")

    def test_resolve_set(self):
        sets, _ = m.parse_page(SETS, "ListSets")
        self.assertEqual(m.resolve_set(sets, "cherokee advocate")["spec"], "ADV")
        with self.assertRaises(m.HarvestError):
            m.resolve_set(sets, "zzz")

    def test_user_agent_is_generic(self):
        ua = m.USER_AGENT
        self.assertNotIn("@", ua)
        self.assertNotIn("Mozilla", ua)
        self.assertIn("research script", ua.replace("-", " "))

    def test_main_end_to_end(self):
        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "o.json")
            fake = Fake([SETS, PAGE1, PAGE2])
            rc = m.main(["--endpoint", "https://example.test/oai/", "--set-name", "Cherokee Advocate", "--out", out],
                        fetch=fake, sleep=lambda s: None)
            self.assertEqual(rc, 0)
            data = json.load(open(out))
            self.assertEqual((data["set_spec"], data["issues"], data["requests"]), ("ADV", 4, 3))
            self.assertEqual(list(data), sorted(data))

    def test_main_stops_on_http_error(self):
        fake = Fake([PAGE1, m.HarvestError("HTTP 429 for u"), PAGE2])
        rc = m.main(["--set", "ADV"], fetch=fake, sleep=lambda s: None)
        self.assertEqual(rc, 1)
        self.assertEqual(len(fake.urls), 2)


if __name__ == "__main__":
    unittest.main()
