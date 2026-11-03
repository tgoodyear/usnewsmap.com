"""Tests for the OCR prefetch manifest builder (#1055): the parts with no network or pyarrow."""
import datetime as dt
import contextlib
import io
import importlib.util
import sys
import unittest
import urllib.error
from pathlib import Path
from unittest import mock
import tempfile
import types
import json

spec = importlib.util.spec_from_file_location("ocr_manifest", Path(__file__).resolve().parent / "ocr_manifest.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "quality-feed"))
import scan_quarantine as quarantine

UTC = dt.timezone.utc


def q(doc, reason, hour, batch="b"):
    return {"doc_id": doc, "batch": batch, "batch_version": 1, "reason": reason, "language": "eng",
            "queued_at": dt.datetime(2026, 10, 24, hour, tzinfo=UTC)}


class PendingTest(unittest.TestCase):
    def test_users_first_and_finished_pages_left_out(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "bulk", 1), q("sn1_1900-01-01_ed-1_seq-2", "user", 5),
                  q("sn1_1900-01-01_ed-1_seq-3", "user", 2), q("sn1_1900-01-01_ed-1_seq-4", "user", 1)]
        outcomes = [{"doc_id": "sn1_1900-01-01_ed-1_seq-4", "outcome": "imported",
                     "at": dt.datetime(2026, 10, 24, 3, tzinfo=UTC)}]
        got = [x["doc_id"][-5:] for x in m.pending(queued, outcomes, ["user", "bulk"])]
        self.assertEqual(got, ["seq-3", "seq-2", "seq-1"])

    def test_a_page_queued_again_after_its_outcome_counts(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "user", 1), q("sn1_1900-01-01_ed-1_seq-1", "user", 6)]
        outcomes = [{"doc_id": "sn1_1900-01-01_ed-1_seq-1", "outcome": "imported",
                     "at": dt.datetime(2026, 10, 24, 3, tzinfo=UTC)}]
        self.assertEqual(len(m.pending(queued, outcomes, ["user"])), 1)

    def test_later_outcomes_end_a_page_but_an_unroutable_dead_letter_does_not(self):
        at = dt.datetime(2026, 10, 24, 3, tzinfo=UTC)
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "user", 1), q("sn1_1900-01-01_ed-1_seq-2", "user", 1),
                  q("sn1_1900-01-01_ed-1_seq-3", "user", 1)]
        outcomes = [
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "outcome": "not_sent", "cause": None, "at": at},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-2", "outcome": "dead_lettered", "cause": "max_dequeues", "at": at},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-3", "outcome": "dead_lettered", "cause": "unroutable", "at": at},
        ]
        got = [x["doc_id"][-5:] for x in m.pending(queued, outcomes, ["user"])]
        self.assertEqual(got, ["seq-3"])

    def test_reasons_not_asked_for_and_bad_ids_are_left_out(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "bulk", 1), q("not a page", "user", 1)]
        self.assertEqual(m.pending(queued, [], ["user"]), [])

    def test_rows_carry_the_manifest_columns(self):
        r = m.row_of(q("sn83030193_1918-01-28_ed-2_seq-1", "user", 1, batch="nn_keddy"),
                     {"sn83030193_1918-01-28_ed-2_seq-1": {"lang": "eng", "decision": "detected", "damage_rate": 0.31}})
        self.assertEqual((r["lccn"], r["edition"], r["seq"], r["damage_rate"]), ("sn83030193", 2, 1, 0.31))
        self.assertEqual(r["item_url"], "https://www.loc.gov/item/sn83030193/1918-01-28/ed-2/")
        r = m.row_of(q("sn1_1900-01-01_ed-1_seq-1", "user", 1), {})
        self.assertEqual((r["lang"], r["decision"], r["damage_rate"]), ("eng", "detected", 0.0))


def ranked(doc, batch, lccn, decade, rank):
    return [doc, batch, "1", "detected", "0.5", lccn, decade, str(rank)]


class PickIssuesTest(unittest.TestCase):
    rows = [
        # Rank 1 of each stratum, worst first: an issue of title a (two pages), one of b.
        ranked("a_1900-01-01_ed-1_seq-1", "x_a_ver01", "a", "1900", 1),
        ranked("a_1900-01-01_ed-1_seq-2", "x_a_ver01", "a", "1900", 1),
        ranked("b_1910-05-01_ed-1_seq-1", "x_b_ver01", "b", "1910", 1),
        # Rank 2: the next issue of each.
        ranked("a_1901-02-02_ed-1_seq-1", "x_a_ver01", "a", "1900", 2),
        ranked("b_1911-05-01_ed-2_seq-1", "x_b_ver01", "b", "1910", 2),
        ranked("b_1911-05-01_ed-2_seq-2", "x_b_ver01", "b", "1910", 2),
    ]

    def docs(self, picked):
        return [r[0] for r in picked]

    def test_whole_issues_round_robin(self):
        got = m.pick_issues(self.rows, set(), set(), strata=1, max_pages=100)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "a_1900-01-01_ed-1_seq-2",
                                          "b_1910-05-01_ed-1_seq-1"])
        got = m.pick_issues(self.rows, set(), set(), strata=2, max_pages=100)
        self.assertEqual(len(got), 6)

    def test_a_done_issue_gives_way_and_done_pages_are_left_out(self):
        queued = {"b_1910-05-01_ed-1_seq-1"}
        index = {("x_a_ver01", "a_1900-01-01_ed-1_seq-2")}
        got = m.pick_issues(self.rows, queued, index, strata=1, max_pages=100)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "b_1911-05-01_ed-2_seq-1",
                                          "b_1911-05-01_ed-2_seq-2"])

    def test_an_issue_is_never_split_by_the_cap(self):
        got = m.pick_issues(self.rows, set(), set(), strata=2, max_pages=4)
        # a's first issue (2), b's first (1), then a's second (1) fits; b's second (2) doesn't.
        self.assertEqual(len(got), 4)
        self.assertNotIn("b_1911-05-01_ed-2_seq-1", self.docs(got))
        got = m.pick_issues(self.rows, set(), set(), strata=1, max_pages=1)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "a_1900-01-01_ed-1_seq-2"])

    def test_batches_file(self):
        import tempfile
        with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as f:
            f.write("# probe\nak_goat_ver01\t200\t2026-10-25T20:51:39Z\nak_albatross_ver01\t403\tt\n"
                    "curiv_joshuaTree_ver01\n")
        self.assertEqual(m.read_batches(f.name), {"ak_goat_ver01", "curiv_joshuaTree_ver01"})
        self.assertEqual(m.batch_pair("ak_arcticfox_ver02"), ("ak_arcticfox", 2))
        with open(f.name, "w") as g:
            g.write("x'; drop table y; --\n")
        with self.assertRaises(SystemExit):
            m.read_batches(f.name)


class NonEnglishTest(unittest.TestCase):
    def test_scored_nonenglish_pages_keep_language_and_page_damage_order(self):
        with tempfile.TemporaryDirectory() as folder:
            a = types.SimpleNamespace(state=folder, batches=None, scan_server="http://scan", dsn="test",
                                      strata=1, slack=20, max_pages=2, non_english=True, what_if=True,
                                      quarantine_root=folder)
            # The SQL itself orders individual pages, not language medians or strata,
            # and deliberately does not exclude already image-scored/staged pages.
            rows = ("sn1_1900-01-02_ed-1_seq-3,xx_test,1,detected,0.8,sn1,1900,srp\n"
                    "sn2_1900-01-02_ed-1_seq-1,xx_other,1,detected,0.6,sn2,1900,rus\n")
            run = types.SimpleNamespace(returncode=0, stdout=rows)
            with mock.patch.object(m, "staged_pairs", return_value={
                ("xx_test", "sn1_1900-01-02_ed-1_seq-3"): {"doc_id": "sn1_1900-01-02_ed-1_seq-3"}}), \
                    mock.patch.object(m, "available", return_value=False) as availability, \
                    mock.patch("subprocess.run", return_value=run) as psql, \
                    mock.patch.object(m, "manifest_bytes", return_value=b"parquet") as manifest:
                m.cmd_build_quality(a)
            query = psql.call_args.kwargs["input"]
            self.assertIn("ORDER BY s.damage_rate DESC NULLS LAST, s.doc_id", query)
            self.assertNotIn("has_image_features", query)
            self.assertNotIn("sn1_1900-01-02_ed-1_seq-3\n\\.", query)
            self.assertEqual(availability.call_args.kwargs["sets"], ("keep",))
            self.assertNotIn("-c", psql.call_args.args[0])
            self.assertEqual([r["lang"] for r in manifest.call_args.args[0]], ["srp", "rus"])
            self.assertEqual([r["damage_rate"] for r in manifest.call_args.args[0]], [0.8, 0.6])
            self.assertFalse(Path(folder, "last-quality.json").exists())

    def test_consumed_history_does_not_count_as_recovered_keep_scan(self):
        with tempfile.TemporaryDirectory() as folder:
            doc = "sn1_1900-01-02_ed-1_seq-3"
            Path(folder, "last-quality.json").write_text(json.dumps({
                "name": "queue-quality-qwen-test", "at": dt.datetime.now(UTC).isoformat(),
                "pairs": [["xx_test", doc]]}))
            a = types.SimpleNamespace(state=folder, batches=None, scan_server="http://scan",
                                      non_english=True, quarantine_root=folder)
            with mock.patch.object(m, "staged_pairs", return_value={("xx_test", doc): {}}), \
                    mock.patch.object(m, "available", return_value=False), \
                    mock.patch("subprocess.run", side_effect=AssertionError("queued ahead of recovery")):
                m.cmd_build_quality(a)


@unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for repair manifests")
class DecodeRepairTest(unittest.TestCase):
    class DecoderError(Exception):
        original_type = "OSError"

    class Response(io.BytesIO):
        def __init__(self, data=b"", status=200):
            super().__init__(data)
            self.status = status

    class ScanServer:
        def __init__(self):
            self.files = {}
            self.requests = []
            self.get_error = None
            self.fail_put_once = False

        def __call__(self, request, timeout):
            method, url = request.get_method(), request.full_url
            self.requests.append((method, url, request.data))
            if method == "PUT":
                if self.fail_put_once:
                    self.fail_put_once = False
                    raise urllib.error.URLError("response lost after publication")
                return DecodeRepairTest.Response(status=201)
            if self.get_error:
                raise self.get_error
            if url not in self.files:
                raise urllib.error.HTTPError(url, 404, "not found", {}, None)
            return DecodeRepairTest.Response(self.files[url])

        def keep(self, page, sha=None):
            scan = page["scan"].replace("buffer/", "keep/", 1)
            url = "http://scan/scans/" + scan
            self.files[url] = b"scan exists"
            self.files[url + ".json"] = json.dumps({
                "doc_id": page["doc_id"], "batch": page["batch"], "batch_version": 1,
                "loc_url": "https://tile.loc.gov/example.jp2", "iiif_base": "https://example.test/iiif",
                "fetched_at": "2026-11-03T03:00:00Z", "download_ms": 100, "user_agent": "test",
                "bytes": page["bytes"], "sha256": sha or page["sha256"],
                "format": "jp2", "resolution": "full", "md5": "b" * 32}).encode()

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.state = self.root / "qwen-test"
        self.server = self.ScanServer()

    @staticmethod
    def page(seq=1, storage="buffer", lang="rus"):
        return {"doc_id": f"sn1_1900-01-02_ed-1_seq-{seq}", "batch": "xx_test", "batch_version": 1,
                "bytes": 12, "sha256": "a" * 64, "lang": lang, "damage_rate": 0.8,
                "scan": f"{storage}/xx_test/sn1/1900-01-02/1/{seq}.jp2"}

    def quarantine(self, page):
        error = self.DecoderError("broken data stream when reading image file")
        for _ in range(3):
            quarantine.record_failure(self.state, page, error, {"pillow": "test", "codec": "jpg_2000"})

    def reconcile(self):
        with mock.patch.object(m.urllib.request, "urlopen", side_effect=self.server), \
                contextlib.redirect_stdout(io.StringIO()):
            m.queue_decode_repairs(self.root, "http://scan")

    def test_buffer_repair_uses_real_single_page_manifest_and_never_deletes_the_source(self):
        import pyarrow as pa
        import pyarrow.parquet as pq

        page = self.page()
        self.quarantine(page)
        self.reconcile()
        requests = [r for r in self.server.requests if r[0] == "PUT"]
        self.assertEqual(len(requests), 1)
        self.assertIn("/manifests/queue-quality-zdecode-", requests[0][1])
        table = pq.read_table(pa.BufferReader(requests[0][2]))
        self.assertEqual(table.schema.metadata[b"usnm.format"], b"usnm-prefetch-decode-repair/1")
        row = table.to_pylist()[0]
        self.assertEqual((row["doc_id"], row["batch"], row["lang"], row["seq"]),
                         (page["doc_id"], page["batch"], "rus", 1))
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "queued")
        self.reconcile()
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 1)
        self.server.keep(page)
        self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "observed_same")
        self.reconcile()
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 1)
        self.assertTrue(all(method in ("GET", "PUT") for method, _, _ in self.server.requests))

    def test_repair_keeps_pending_on_network_error_and_replays_identical_manifest_after_lost_ack(self):
        page = self.page()
        self.quarantine(page)
        self.server.get_error = urllib.error.URLError("scan server timeout")
        with self.assertRaises(urllib.error.URLError):
            self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "pending")
        self.server.get_error = None
        self.server.fail_put_once = True
        with self.assertRaises(urllib.error.URLError):
            self.reconcile()
        saved = quarantine.quarantined(self.state)[0]
        self.assertEqual(saved["refetch_state"], "prepared")
        self.reconcile()
        puts = [r for r in self.server.requests if r[0] == "PUT"]
        self.assertEqual(len(puts), 2)
        self.assertEqual(puts[0][1:], puts[1][1:])
        self.assertEqual(saved["manifest_body"], puts[1][2])
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "queued")

    def test_cpu_manual_quarantine_is_excluded_but_never_automatically_repaired(self):
        cpu, qwen = self.page(1), self.page(2)
        cpu_state = self.root / "cpu-test"
        error = self.DecoderError("broken data stream when reading image file")
        for _ in range(3):
            quarantine.record_failure(cpu_state, cpu, error, {"pillow": "test"}, automatic_repair=False)
        self.quarantine(qwen)
        self.assertEqual(quarantine.quarantined_documents(self.root), {cpu["doc_id"], qwen["doc_id"]})
        self.reconcile()
        self.assertEqual(quarantine.quarantined(cpu_state)[0]["refetch_state"], "manual_review")
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "queued")
        puts = [r for r in self.server.requests if r[0] == "PUT"]
        self.assertEqual(len(puts), 1)
        import pyarrow as pa
        import pyarrow.parquet as pq

        self.assertEqual(pq.read_table(pa.BufferReader(puts[0][2])).to_pylist()[0]["doc_id"], qwen["doc_id"])

    def test_existing_keep_and_english_sources_are_blocked_without_mutation(self):
        kept, occupied, english = self.page(1, "keep"), self.page(2), self.page(3, lang="eng")
        for page in (kept, occupied, english):
            self.quarantine(page)
        url = "http://scan/scans/" + occupied["scan"].replace("buffer/", "keep/", 1)
        self.server.files[url] = b"orphaned retained scan"
        self.reconcile()
        statuses = {row["source"]["doc_id"]: row["refetch_state"] for row in quarantine.quarantined(self.state)}
        self.assertEqual(statuses, {kept["doc_id"]: "blocked_keep", occupied["doc_id"]: "blocked_keep",
                                    english["doc_id"]: "blocked_language"})
        self.assertFalse(any(method != "GET" for method, _, _ in self.server.requests))
        self.assertEqual(self.server.files[url], b"orphaned retained scan")

    def test_existing_changed_keep_source_is_observed_not_overwritten(self):
        page = self.page()
        self.quarantine(page)
        self.server.keep(page, "b" * 64)
        self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "observed_new")
        self.assertFalse(any(method == "PUT" for method, _, _ in self.server.requests))

    def test_malformed_keep_sidecars_are_blocked_without_stopping_other_repairs(self):
        bad_sidecars = (b"{", b"[]", {"batch_version": "bad"}, {"sha256": 42},
                        b"[" * 2000 + b"]" * 2000, "oversized")
        for seq, bad in enumerate(bad_sidecars, 1):
            page = self.page(seq)
            self.quarantine(page)
            self.server.keep(page)
            url = "http://scan/scans/" + page["scan"].replace("buffer/", "keep/", 1) + ".json"
            if isinstance(bad, dict):
                bad = json.dumps({**json.loads(self.server.files[url]), **bad}).encode()
            elif bad == "oversized":
                valid = self.server.files[url]
                bad = valid + b" " * (65537 - len(valid))
            self.server.files[url] = bad
        safe = self.page(len(bad_sidecars) + 1)
        self.quarantine(safe)
        self.reconcile()
        statuses = {row["source"]["doc_id"]: row["refetch_state"] for row in quarantine.quarantined(self.state)}
        self.assertEqual([statuses[self.page(seq)["doc_id"]] for seq in range(1, len(bad_sidecars) + 1)],
                         ["blocked_keep"] * len(bad_sidecars))
        self.assertEqual(statuses[safe["doc_id"]], "queued")
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 1)

    def test_invalid_queued_sidecars_do_not_release_outstanding_repair_slots(self):
        pages = [self.page(seq) for seq in range(1, 11)]
        for page in pages:
            self.quarantine(page)
        self.reconcile()
        changes = (
            {"batch_version": True}, {"batch_version": 1.9}, {"batch_version": "1"},
            {"batch_version": None}, "missing_version", "missing_url", {"fetched_at": "invalid"},
            "missing_md5", {"unexpected_field": "x"}, "duplicate_version",
        )
        for page, change in zip(pages, changes):
            self.server.keep(page)
            url = "http://scan/scans/" + page["scan"].replace("buffer/", "keep/", 1) + ".json"
            side = json.loads(self.server.files[url])
            if change == "missing_version":
                del side["batch_version"]
            elif change == "missing_url":
                del side["loc_url"]
            elif change == "missing_md5":
                del side["md5"]
            elif isinstance(change, dict):
                side.update(change)
            raw = json.dumps(side).encode()
            if change == "duplicate_version":
                raw = b'{"batch_version": false,' + raw[1:]
            self.server.files[url] = raw
        waiting = self.page(11)
        self.quarantine(waiting)
        self.reconcile()
        statuses = {row["source"]["doc_id"]: row["refetch_state"] for row in quarantine.quarantined(self.state)}
        self.assertEqual([statuses[page["doc_id"]] for page in pages], ["queued"] * 10)
        self.assertEqual(statuses[waiting["doc_id"]], "pending")
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 10)

        self.server.keep(pages[0])
        url = "http://scan/scans/" + pages[0]["scan"].replace("buffer/", "keep/", 1) + ".json"
        self.server.files[url] += b" " * (65536 - len(self.server.files[url]))
        self.reconcile()
        statuses = {row["source"]["doc_id"]: row["refetch_state"] for row in quarantine.quarantined(self.state)}
        self.assertEqual(statuses[pages[0]["doc_id"]], "observed_same")
        self.assertEqual(statuses[waiting["doc_id"]], "queued")
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 11)

    def test_partial_keep_write_after_queueing_is_rechecked_when_sidecar_arrives(self):
        page = self.page()
        self.quarantine(page)
        self.reconcile()
        url = "http://scan/scans/" + page["scan"].replace("buffer/", "keep/", 1)
        self.server.files[url] = b"incomplete keep write"
        self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "queued")
        self.server.keep(page, "b" * 64)
        self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "observed_new")
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 1)

    def test_legacy_jpeg_sidecar_without_format_or_md5_is_observed(self):
        page = self.page()
        page["scan"] = page["scan"].replace(".jp2", ".jpg")
        self.quarantine(page)
        self.reconcile()
        self.server.keep(page)
        url = "http://scan/scans/" + page["scan"].replace("buffer/", "keep/", 1) + ".json"
        side = json.loads(self.server.files[url])
        del side["format"], side["md5"]
        self.server.files[url] = json.dumps(side).encode()
        self.reconcile()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "observed_same")
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 1)

    def test_what_if_never_reconciles_or_changes_quarantine(self):
        page = self.page()
        self.quarantine(page)
        args = types.SimpleNamespace(state=str(self.root / "quality"), dsn="x", scan_server="http://scan",
                                     max_pages=10, strata=1, batches=None, non_english=True,
                                     what_if=True, quarantine_root=str(self.root), queue_decode_repairs="on")
        with mock.patch.object(m, "queue_decode_repairs") as repair, \
                mock.patch("subprocess.run", return_value=types.SimpleNamespace(returncode=0, stdout="", stderr="")), \
                mock.patch.object(m, "staged_pairs", return_value={}), \
                contextlib.redirect_stdout(io.StringIO()):
            m.cmd_build_quality(args)
        repair.assert_not_called()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "pending")
        self.assertEqual(self.server.requests, [])

    def test_quarantines_are_excluded_from_ordinary_acquisition_even_when_repairs_are_off(self):
        blocked, healthy = self.page(), self.page(2)
        self.quarantine(blocked)
        self.server.files["http://scan/scans/" + blocked["scan"].replace("buffer/", "keep/", 1)] = b"orphan"
        candidates = "".join(f"{page['doc_id']},xx_test,1,detected,0.8,sn1,1900,rus\n"
                             for page in (blocked, healthy))
        for what_if in (True, False):
            with self.subTest(what_if=what_if), \
                    mock.patch.object(m, "queue_decode_repairs") as repair, \
                    mock.patch.object(m, "staged_pairs", return_value={}), \
                    mock.patch.object(m, "manifest_bytes", wraps=m.manifest_bytes) as manifest, \
                    mock.patch.object(m.urllib.request, "urlopen", side_effect=self.server), \
                    mock.patch("subprocess.run", return_value=types.SimpleNamespace(
                        returncode=0, stdout=candidates, stderr="")) as psql, \
                    contextlib.redirect_stdout(io.StringIO()):
                args = types.SimpleNamespace(state=str(self.root / f"quality-{what_if}"), dsn="x",
                                             scan_server="http://scan", max_pages=10, strata=1,
                                             batches=None, non_english=True, what_if=what_if,
                                             quarantine_root=str(self.root), queue_decode_repairs="off")
                m.cmd_build_quality(args)
                self.assertIn(blocked["doc_id"] + "\n", psql.call_args.kwargs["input"])
                self.assertEqual([row["doc_id"] for row in manifest.call_args.args[0]], [healthy["doc_id"]])
                repair.assert_not_called()
        self.assertEqual(quarantine.quarantined(self.state)[0]["refetch_state"], "pending")
        puts = [r for r in self.server.requests if r[0] == "PUT"]
        self.assertEqual(len(puts), 1)
        self.assertIn("/manifests/queue-quality-qwen-", puts[0][1])
        import pyarrow as pa
        import pyarrow.parquet as pq

        table = pq.read_table(pa.BufferReader(puts[0][2]))
        self.assertEqual(table.schema.metadata[b"usnm.format"], b"usnm-prefetch-manifest/1")
        self.assertEqual([row["doc_id"] for row in table.to_pylist()], [healthy["doc_id"]])

    def test_nonenglish_acquisition_requires_readable_quarantine_root_even_when_repairs_are_off(self):
        args = types.SimpleNamespace(state=str(self.root / "quality"), scan_server="http://scan",
                                     non_english=True, quarantine_root=str(self.root / "missing"),
                                     queue_decode_repairs="off")
        with self.assertRaises(FileNotFoundError):
            m.cmd_build_quality(args)

    def test_repairs_run_only_when_explicitly_enabled(self):
        args = types.SimpleNamespace(state=str(self.root / "quality"), dsn="x", scan_server="http://scan",
                                     max_pages=10, strata=1, batches=None, non_english=True,
                                     what_if=False, quarantine_root=str(self.root), queue_decode_repairs="on")
        with mock.patch.object(m, "queue_decode_repairs") as repair, \
                mock.patch("subprocess.run", return_value=types.SimpleNamespace(returncode=0, stdout="", stderr="")), \
                mock.patch.object(m, "staged_pairs", return_value={}), \
                contextlib.redirect_stdout(io.StringIO()):
            m.cmd_build_quality(args)
        repair.assert_called_once_with(str(self.root), "http://scan")

    def test_missing_quarantine_root_fails_visibly(self):
        with self.assertRaises(FileNotFoundError):
            m.queue_decode_repairs(self.root / "missing", "http://scan")

    def test_refetch_backlog_is_bounded_until_a_keep_copy_is_observed(self):
        pages = [self.page(seq) for seq in range(1, 12)]
        for page in pages:
            self.quarantine(page)
        self.reconcile()
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 10)
        self.reconcile()
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 10)
        queued = next(row for row in quarantine.quarantined(self.state) if row["refetch_state"] == "queued")
        page = next(page for page in pages if page["doc_id"] == queued["source"]["doc_id"])
        self.server.keep(page)
        self.reconcile()
        self.assertEqual(len([r for r in self.server.requests if r[0] == "PUT"]), 11)


if __name__ == "__main__":
    unittest.main()
