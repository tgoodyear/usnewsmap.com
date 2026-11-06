"""Tests for the OCR prefetch manifest builder (#1055)."""
import contextlib
import datetime as dt
import importlib.util
import io
import threading
import unittest
from pathlib import Path
from unittest import mock
import tempfile
import types
import json

spec = importlib.util.spec_from_file_location("ocr_manifest", Path(__file__).resolve().parent / "ocr_manifest.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
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


class OcrClaimTest(unittest.TestCase):
    def test_queued_pages_already_on_the_scan_server_are_claimed_even_while_a_manifest_is_fetched(self):
        on, off, ended = "sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2", "sn1_1900-01-01_ed-1_seq-3"
        with tempfile.TemporaryDirectory() as d:
            Path(d, "last.json").write_text(json.dumps({
                "name": "queue-ocr-x", "at": dt.datetime.now(UTC).isoformat(), "pairs": [["b", off]]}))
            Path(d, "ocr-docs.txt").write_text(f"{off}\n{ended}\n")
            a = types.SimpleNamespace(state=d, reasons="user,bulk", scan_server="http://s", curated_url="http://c",
                                      days=14, max_pages=10, curated_copy=d, what_if=False)
            # ended's scan is on the server too, but OCR has finished with it: not claimed, and
            # no longer active though OCR once asked for it.
            rows = {"queued": [q(on, "user", 1), q(off, "user", 2), q(ended, "user", 3)],
                    "outcomes": [{"doc_id": ended, "outcome": "not_sent", "cause": None,
                                  "at": dt.datetime(2026, 10, 24, 4, tzinfo=UTC)}]}
            refreshed = []

            def cached(lake, state, kind, *a):
                refreshed.append((kind, lake is not None))
                return rows[kind]

            index = {("b", d): {"doc_id": d, "batch": "b"} for d in (on, ended)}
            with mock.patch.object(m, "staged_pairs", return_value=index), \
                    mock.patch.object(m, "Lake"), \
                    mock.patch.object(m, "cached_rows", side_effect=cached), \
                    contextlib.redirect_stdout(io.StringIO()) as out:
                m.cmd_build(a)
                m.cmd_build(a)
            self.assertEqual(Path(d, "ocr-docs.txt").read_text().split(), [off, ended, on])
            self.assertEqual(Path(d, "ocr-active.txt").read_text().split(), sorted([off, on]))
            self.assertIn("still being fetched", out.getvalue())
            # Claims see what was queued, and what OCR finished, since the manifest went out.
            self.assertEqual(refreshed, [("queued", True), ("outcomes", True)] * 2)

    def test_a_new_manifest_s_pages_are_active_claims_at_once(self):
        doc = "sn1_1900-01-01_ed-1_seq-4"
        with tempfile.TemporaryDirectory() as d:
            a = types.SimpleNamespace(state=d, reasons="user", scan_server="http://s", curated_url="http://c",
                                      days=14, max_pages=10, curated_copy=d, what_if=False)
            resp = mock.MagicMock(status=201)
            resp.__enter__.return_value = resp
            with mock.patch.object(m, "staged_pairs", return_value={}), \
                    mock.patch.object(m, "Lake"), \
                    mock.patch.object(m, "cached_rows",
                                      side_effect=lambda lake, state, kind, *x: [q(doc, "user", 1)] if kind == "queued" else []), \
                    mock.patch.object(m, "damage_of", return_value={}), \
                    mock.patch.object(m, "manifest_bytes", return_value=b"x"), \
                    mock.patch.object(m.urllib.request, "urlopen", return_value=resp), \
                    contextlib.redirect_stdout(io.StringIO()):
                m.cmd_build(a)
            self.assertEqual(Path(d, "ocr-docs.txt").read_text().split(), [doc])
            self.assertEqual(Path(d, "ocr-active.txt").read_text().split(), [doc])

    def test_claims_are_published_under_the_quality_follower_s_lock(self):
        on = "sn1_1900-01-01_ed-1_seq-1"
        with tempfile.TemporaryDirectory() as d:
            lock = Path(d, "follower/kept.tsv.lock")
            a = types.SimpleNamespace(state=d, reasons="user", scan_server="http://s", curated_url="http://c",
                                      days=14, max_pages=10, curated_copy=d, what_if=False, claims_lock=str(lock))
            # A manifest still being fetched: only the claim of on, already on the scan server.
            Path(d, "last.json").write_text(json.dumps({
                "name": "queue-ocr-x", "at": dt.datetime.now(UTC).isoformat(),
                "pairs": [["b", "sn1_1900-01-01_ed-1_seq-2"]]}))
            waiting, real = threading.Event(), m.fcntl.flock

            def flock(f, op):
                waiting.set()
                return real(f, op)

            lock.parent.mkdir()
            with open(lock, "a") as held, \
                    mock.patch.object(m, "staged_pairs", return_value={("b", on): {"doc_id": on, "batch": "b"}}), \
                    mock.patch.object(m, "Lake"), \
                    mock.patch.object(m, "cached_rows", side_effect=lambda lake, state, kind, *x:
                                      [q(on, "user", 1)] if kind == "queued" else []), \
                    mock.patch.object(m.fcntl, "flock", flock), \
                    contextlib.redirect_stdout(io.StringIO()):
                real(held, m.fcntl.LOCK_EX)
                t = threading.Thread(target=m.cmd_build, args=(a,))
                t.start()
                self.assertTrue(waiting.wait(5))
                self.assertFalse(Path(d, "ocr-docs.txt").exists() or Path(d, "ocr-active.txt").exists())
                real(held, m.fcntl.LOCK_UN)
                t.join(10)
            self.assertEqual(Path(d, "ocr-active.txt").read_text().split(), [on])


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


class QuarantineExclusionTest(unittest.TestCase):
    class DecoderError(Exception):
        original_type = "OSError"

    @staticmethod
    def page(seq):
        return {"doc_id": f"sn1_1900-01-02_ed-1_seq-{seq}", "batch": "xx_test", "batch_version": 1,
                "bytes": 12, "sha256": "a" * 64, "lang": "rus", "damage_rate": 0.8,
                "scan": f"buffer/xx_test/sn1/1900-01-02/1/{seq}.jp2"}

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for manifest compatibility")
    def test_quarantines_are_excluded_and_only_an_ordinary_manifest_is_published(self):
        import pyarrow as pa
        import pyarrow.parquet as pq

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            quarantine = m.load_scan_quarantine()
            blocked = [self.page(1), self.page(2)]
            for campaign, page in zip(("cpu-test", "qwen-test"), blocked):
                for _ in range(3):
                    quarantine.record_failure(root / campaign, page, self.DecoderError("broken stream"),
                                              {"pillow": "test", "codec": "jpg_2000"})
                record = quarantine.quarantined(root / campaign)[0]
                self.assertEqual(record["refetch_state"], "manual_review")
                self.assertIsNone(record["manifest_body"])

            healthy = self.page(3)
            candidates = "".join(f"{page['doc_id']},xx_test,1,detected,0.8,sn1,1900,rus\n"
                                 for page in [*blocked, healthy])
            requests = []

            def urlopen(request, timeout):
                requests.append(request)
                response = io.BytesIO()
                response.status = 201
                return response

            for what_if in (True, False):
                args = types.SimpleNamespace(state=str(root / f"builder-{what_if}"), dsn="test",
                                             scan_server="http://scan", max_pages=10, strata=1,
                                             batches=None, non_english=True, what_if=what_if,
                                             quarantine_root=folder)
                with self.subTest(what_if=what_if), \
                        mock.patch.object(m, "staged_pairs", return_value={}), \
                        mock.patch.object(m.urllib.request, "urlopen", side_effect=urlopen), \
                        mock.patch("subprocess.run", return_value=types.SimpleNamespace(
                            returncode=0, stdout=candidates, stderr="")) as psql, \
                        contextlib.redirect_stdout(io.StringIO()):
                    m.cmd_build_quality(args)
                    for page in blocked:
                        self.assertIn(page["doc_id"] + "\n", psql.call_args.kwargs["input"])
            self.assertEqual(len(requests), 1)
            self.assertEqual(requests[0].get_method(), "PUT")
            self.assertIn("/manifests/queue-quality-qwen-", requests[0].full_url)
            table = pq.read_table(pa.BufferReader(requests[0].data))
            self.assertEqual(table.schema.metadata[b"usnm.format"], b"usnm-prefetch-manifest/1")
            self.assertEqual([row["doc_id"] for row in table.to_pylist()], [healthy["doc_id"]])

    def test_quarantine_appearing_during_selection_is_excluded_before_publication(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            blocked, healthy = self.page(1), self.page(2)
            quarantine = m.load_scan_quarantine()
            error = self.DecoderError("broken stream")
            for _ in range(2):
                quarantine.record_failure(root / "qwen-test", blocked, error, {"pillow": "test"})
            candidates = "".join(f"{page['doc_id']},xx_test,1,detected,0.8,sn1,1900,rus\n"
                                 for page in (blocked, healthy))
            read_quarantines, reads = quarantine.quarantined_documents, []

            def observe(path):
                result = read_quarantines(path)
                reads.append(result)
                if len(reads) == 2:
                    quarantine.record_failure(root / "qwen-test", blocked, error, {"pillow": "test"})
                return result

            response = io.BytesIO()
            response.status = 201
            args = types.SimpleNamespace(state=str(root / "builder"), dsn="test", scan_server="http://scan",
                                         max_pages=10, strata=1, batches=None, non_english=True,
                                         what_if=False, quarantine_root=folder)
            with mock.patch.object(m, "load_scan_quarantine", return_value=quarantine), \
                    mock.patch.object(quarantine, "quarantined_documents", side_effect=observe), \
                    mock.patch.object(m, "staged_pairs", return_value={}), \
                    mock.patch.object(m, "manifest_bytes", return_value=b"parquet") as manifest, \
                    mock.patch.object(m.urllib.request, "urlopen", return_value=response) as publish, \
                    mock.patch("subprocess.run", return_value=types.SimpleNamespace(
                        returncode=0, stdout=candidates, stderr="")), \
                    contextlib.redirect_stdout(io.StringIO()):
                m.cmd_build_quality(args)
            self.assertEqual([row["doc_id"] for row in manifest.call_args.args[0]], [healthy["doc_id"]])
            self.assertEqual(publish.call_count, 1)
            self.assertEqual(reads, [set(), set(), {blocked["doc_id"]}])

    def test_quarantine_transition_waits_for_inflight_manifest_publication(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            page = self.page(1)
            quarantine = m.load_scan_quarantine()
            error = self.DecoderError("broken stream")
            for _ in range(2):
                quarantine.record_failure(root / "cpu-test", page, error, {"pillow": "test"})
            candidates = f"{page['doc_id']},xx_test,1,detected,0.8,sn1,1900,rus\n"
            args = types.SimpleNamespace(state=str(root / "builder"), dsn="test", scan_server="http://scan",
                                         max_pages=10, strata=1, batches=None, non_english=True,
                                         what_if=False, quarantine_root=folder)
            put_entered, release_put, writer_started, writer_done = (threading.Event() for _ in range(4))
            errors, requests = [], []

            def urlopen(request, timeout):
                requests.append(request)
                put_entered.set()
                if not release_put.wait(5):
                    raise TimeoutError("test did not release manifest PUT")
                response = io.BytesIO()
                response.status = 201
                return response

            def build():
                try:
                    m.cmd_build_quality(args)
                except BaseException as exc:
                    errors.append(exc)

            def record():
                writer_started.set()
                try:
                    quarantine.record_failure(root / "cpu-test", page, error, {"pillow": "test"})
                except BaseException as exc:
                    errors.append(exc)
                finally:
                    writer_done.set()

            builder = threading.Thread(target=build, daemon=True)
            writer = threading.Thread(target=record, daemon=True)
            with mock.patch.object(m, "staged_pairs", return_value={}), \
                    mock.patch.object(m, "manifest_bytes", return_value=b"parquet"), \
                    mock.patch.object(m.urllib.request, "urlopen", side_effect=urlopen), \
                    mock.patch("subprocess.run", return_value=types.SimpleNamespace(
                        returncode=0, stdout=candidates, stderr="")), \
                    contextlib.redirect_stdout(io.StringIO()):
                builder.start()
                try:
                    self.assertTrue(put_entered.wait(5), "builder did not reach manifest PUT")
                    writer.start()
                    self.assertTrue(writer_started.wait(5))
                    self.assertFalse(writer_done.wait(0.3), "quarantine committed during manifest publication")
                finally:
                    release_put.set()
                    builder.join(5)
                    if writer_started.is_set():
                        writer.join(5)
            self.assertFalse(builder.is_alive())
            self.assertFalse(writer.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(len(requests), 1)
            self.assertEqual(quarantine.quarantined(root / "cpu-test")[0]["refetch_state"], "manual_review")

    def test_failed_manifest_publication_releases_quarantine_lock(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            page = self.page(1)
            quarantine = m.load_scan_quarantine()
            error = self.DecoderError("broken stream")
            for _ in range(2):
                quarantine.record_failure(root / "cpu-test", page, error, {"pillow": "test"})
            candidates = f"{page['doc_id']},xx_test,1,detected,0.8,sn1,1900,rus\n"
            args = types.SimpleNamespace(state=str(root / "builder"), dsn="test", scan_server="http://scan",
                                         max_pages=10, strata=1, batches=None, non_english=True,
                                         what_if=False, quarantine_root=folder)
            with mock.patch.object(m, "staged_pairs", return_value={}), \
                    mock.patch.object(m, "manifest_bytes", return_value=b"parquet"), \
                    mock.patch.object(m.urllib.request, "urlopen", side_effect=OSError("network down")), \
                    mock.patch("subprocess.run", return_value=types.SimpleNamespace(
                        returncode=0, stdout=candidates, stderr="")), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    self.assertRaisesRegex(OSError, "network down"):
                m.cmd_build_quality(args)
            writer = threading.Thread(target=quarantine.record_failure,
                                      args=(root / "cpu-test", page, error, {"pillow": "test"}), daemon=True)
            writer.start()
            writer.join(5)
            self.assertFalse(writer.is_alive(), "failed publication kept the quarantine lock")
            self.assertEqual(quarantine.quarantined(root / "cpu-test")[0]["refetch_state"], "manual_review")

    def test_nonenglish_acquisition_requires_readable_quarantine_root(self):
        with tempfile.TemporaryDirectory() as folder:
            args = types.SimpleNamespace(state=folder, scan_server="http://scan", non_english=True,
                                         quarantine_root=str(Path(folder) / "missing"))
            with self.assertRaises(FileNotFoundError):
                m.cmd_build_quality(args)

    def test_repair_queue_option_is_rejected(self):
        with contextlib.redirect_stderr(io.StringIO()) as output, self.assertRaises(SystemExit) as error:
            m.main(["build-quality", "--state", "unused", "--dsn", "unused", "--non-english",
                    "--queue-decode-repairs", "on"])
        self.assertEqual(error.exception.code, 2)
        self.assertIn("--queue-decode-repairs", output.getvalue())


if __name__ == "__main__":
    unittest.main()
