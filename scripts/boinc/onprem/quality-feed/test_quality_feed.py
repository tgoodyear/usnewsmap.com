"""Tests for the on-prem quality feeder (#962), with no network or production state."""
import contextlib
from concurrent.futures import ThreadPoolExecutor
import hashlib
import importlib.util
import io
import json
import os
import sqlite3
import sys
import tempfile
import types
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


feed = load("quality_feed", "quality_feed.py")
receive = load("quality_receive", "quality_receive.py")
loader = load("quality_load", "../../../onprem/quality/load.py")


class FeedTest(unittest.TestCase):
    def test_scan_path_follows_the_keep_layout(self):
        row = {"doc_id": "sn88084068_1917-11-01_ed-1_seq-12", "batch": "arhi_cosmic", "format": "jp2"}
        self.assertEqual(feed.scan_path(row), "keep/arhi_cosmic/sn88084068/1917-11-01/1/12.jp2")
        row.pop("format")
        self.assertTrue(feed.scan_path(row).endswith("/12.jpg"))

    def test_sample_row_matches_the_runner(self):
        p = {"batch": "rp_barbeddevil", "batch_version": 2, "damage_rate": 0.62, "date": "1894-08-01",
             "doc_id": "sn92063918_1894-08-01_ed-1_seq-2", "lang": "spa"}
        r = feed.sample_row(p)
        self.assertEqual((r["damage"], r["decade"], r["prefix"], r["year"]), ("high", 1890, "rp", 1894))
        p["damage_rate"] = None
        self.assertEqual(feed.sample_row(p)["damage_rate"], 0.15)

    def test_languages_and_readers(self):
        self.assertEqual(feed.lang_of("ger"), "deu")
        self.assertEqual(feed.lang_of("xxx"), "eng")
        self.assertEqual(feed.reader_name("surya 0.17", ""), "ours-surya")
        self.assertEqual(feed.reader_name("tesseract 5.5", "ocr-out"), "ours-tesseract")

    def test_empty_alto_carries_the_page_size(self):
        self.assertIn(b'WIDTH="4952" HEIGHT="6341"', feed.empty_alto(4952, 6341))

    def test_text_is_cleaned_and_capped(self):
        self.assertEqual(feed.normalize_text("a\x00b\nc"), "ab\nc")
        self.assertEqual(len(feed.normalize_text("x" * 600_000).encode()), 500_000)

    def test_chunk_is_written_with_sums_and_ready_last(self):
        with tempfile.TemporaryDirectory() as d:
            p = {"batch": "b", "batch_version": 1, "damage_rate": 0.3, "date": "1900-01-01",
                 "doc_id": "sn1_1900-01-01_ed-1_seq-1", "lang": "eng",
                 "bytes": 6, "sha256": hashlib.sha256(b"source").hexdigest()}
            texts = {"page": None, "readers": [{"name": "loc-hist", "text": "x"}]}
            feed.write_chunk(d, "c00001", [p], {p["doc_id"]: (b"jpg", b"<alto/>", texts, None)})
            c = Path(d, "c00001")
            self.assertEqual(c.joinpath("READY").read_text(), "1\n")
            self.assertEqual(feed.pending_chunks(d), [str(c)])
            sums = c.joinpath("SHA256SUMS").read_text().split("\n")
            self.assertIn(f"scans/{p['doc_id']}.jpg", " ".join(sums))
            receipt = json.loads(c.joinpath("texts", p["doc_id"] + ".image-provenance.json").read_text())
            self.assertEqual(receipt["source"]["sha256"], p["sha256"])
            self.assertEqual(receipt["jpeg"]["sha256"], hashlib.sha256(b"jpg").hexdigest())
            self.assertIn(f"texts/{p['doc_id']}.image-provenance.json", " ".join(sums))
            # The receiver accepts it.
            share = Path(d, "share")
            inc = share / "quality" / "v1-test" / "incoming"
            inc.mkdir(parents=True)
            os.rename(c, inc / "c00001")
            (share / "quality" / "v1-wave9").mkdir()
            (share / "quality" / "v1-wave9" / "places.txt").write_text("Albany\n")
            buf = io.StringIO()
            with mock.patch.object(receive.os, "lchown"), contextlib.redirect_stdout(buf):
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", str(share)])
            self.assertEqual(buf.getvalue().split("\t")[1:], ["eng", "1\n"])
            self.assertTrue((share / "quality" / "v1-test" / "scans" / f"{p['doc_id']}.jpg").exists())

    def test_receiver_refuses_a_page_without_its_scan(self):
        with tempfile.TemporaryDirectory() as d:
            inc = Path(d, "quality", "v1-test", "incoming", "c00001")
            (inc / "scans").mkdir(parents=True)
            (inc / "texts").mkdir()
            (inc / "READY").write_text("1\n")
            (inc / "sample.jsonl").write_text(json.dumps({"doc_id": "sn1_1900-01-01_ed-1_seq-1"}) + "\n")
            (inc / "SHA256SUMS").write_text("")
            with self.assertRaises(SystemExit) as e:
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", d])
            self.assertIn("would ask LoC", str(e.exception))


    def test_after_submit_frees_copies_once_every_bundle_is_submitted(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "quality", "v1-test")
            doc = "sn1_1900-01-01_ed-1_seq-1"
            inc = camp / "incoming" / "c00001"
            inc.mkdir(parents=True)
            (inc / "sample.jsonl").write_text(json.dumps({"doc_id": doc}) + "\n")
            for rel in (f"scans/{doc}.jpg", f"texts/{doc}.json", f"pages/{doc}/page.jpg", f"pages/{doc}/quality.json",
                        f"units/bundle-00001-{doc}/page.jpg"):
                (camp / rel).parent.mkdir(parents=True, exist_ok=True)
                (camp / rel).write_text("x")
            (camp / f"units/bundle-00001-{doc}/quality.json").write_text(
                json.dumps({"doc_id": doc, "pages": [{"doc_id": doc}]}))
            args = ["--campaign", "v1-test", "--chunk", "c00001", "--share", d, "--after-submit"]
            with self.assertRaises(SystemExit):
                receive.main(args)
            (camp / f"units/bundle-00001-{doc}/submitted").write_text("wu\n")
            with contextlib.redirect_stdout(io.StringIO()):
                receive.main(args)
            self.assertFalse((camp / f"scans/{doc}.jpg").exists())
            self.assertFalse((camp / f"pages/{doc}/page.jpg").exists())
            self.assertTrue((camp / f"pages/{doc}/quality.json").exists())
            self.assertTrue((camp / f"units/bundle-00001-{doc}/page.jpg").exists())
            self.assertFalse(inc.exists())

    def test_after_submit_skips_a_partial_bundle_and_keeps_the_next_chunks_pages(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "quality", "v1-test")
            doc, shared = "sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2"
            inc = camp / "incoming" / "c00001"
            ahead = camp / "incoming" / "c00002"
            for chunk, docs in ((inc, (doc, shared)), (ahead, (shared,))):
                chunk.mkdir(parents=True)
                (chunk / "sample.jsonl").write_text("".join(json.dumps({"doc_id": x}) + "\n" for x in docs))
            for rel in (f"scans/{doc}.jpg", f"scans/{shared}.jpg", f"texts/{shared}.json",
                        f"units/bundle-00001-{doc}/submitted",
                        # A crash stopped packing before quality.json: the next stage packs again.
                        f"units/bundle-00002-{doc}/page.jpg"):
                (camp / rel).parent.mkdir(parents=True, exist_ok=True)
                (camp / rel).write_text("x")
            (camp / f"units/bundle-00001-{doc}/quality.json").write_text(
                json.dumps({"doc_id": doc, "pages": [{"doc_id": doc}, {"doc_id": shared}]}))
            with contextlib.redirect_stdout(io.StringIO()):
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", d, "--after-submit"])
            self.assertFalse((camp / f"scans/{doc}.jpg").exists())
            # The chunk received ahead holds it too: its stage must find it here, not at LoC.
            self.assertTrue((camp / f"scans/{shared}.jpg").exists())
            self.assertTrue((camp / f"texts/{shared}.json").exists())
            self.assertFalse(inc.exists())
            self.assertTrue(ahead.exists())

    def test_after_submit_frees_nothing_while_a_page_is_in_no_submitted_unit(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "quality", "v1-test")
            doc, older, gone = "sn1_1900-01-01_ed-1_seq-1", "sn1_1899-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2"
            inc = camp / "incoming" / "c00002"
            inc.mkdir(parents=True)
            (inc / "sample.jsonl").write_text("".join(json.dumps({"doc_id": x}) + "\n" for x in (doc, gone)))
            # An older chunk's bundle is submitted; this chunk's lost its quality.json after staging.
            old = camp / f"units/bundle-00001-{older}"
            old.mkdir(parents=True)
            (old / "quality.json").write_text(json.dumps({"doc_id": older, "pages": [{"doc_id": older}]}))
            (old / "submitted").write_text("wu\n")
            (camp / f"units/bundle-00002-{doc}").mkdir(parents=True)
            (camp / f"scans/{doc}.jpg").parent.mkdir(parents=True)
            (camp / f"scans/{doc}.jpg").write_text("x")
            (camp / f"pages/{gone}").mkdir(parents=True)
            (camp / f"pages/{gone}/missing").write_text("LoC has no such page\n")
            with self.assertRaises(SystemExit) as e:
                receive.main(["--campaign", "v1-test", "--chunk", "c00002", "--share", d, "--after-submit"])
            self.assertIn("in no submitted unit", str(e.exception))
            self.assertTrue((camp / f"scans/{doc}.jpg").exists())
            self.assertTrue(inc.exists())

    def test_after_submit_with_a_cut_short_neighbour_keeps_its_copies_and_finishes(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "quality", "v1-test")
            doc = "sn1_1900-01-01_ed-1_seq-1"
            inc = camp / "incoming" / "c00001"
            ahead = camp / "incoming" / "c00002"
            inc.mkdir(parents=True)
            ahead.mkdir(parents=True)
            (inc / "sample.jsonl").write_text(json.dumps({"doc_id": doc}) + "\n")
            (ahead / "sample.jsonl").write_text('{"doc_i')
            for rel in (f"scans/{doc}.jpg", f"units/bundle-00001-{doc}/submitted"):
                (camp / rel).parent.mkdir(parents=True, exist_ok=True)
                (camp / rel).write_text("x")
            (camp / f"units/bundle-00001-{doc}/quality.json").write_text(json.dumps({"doc_id": doc, "pages": []}))
            with contextlib.redirect_stdout(io.StringIO()):
                receive.main(["--campaign", "v1-test", "--chunk", "c00001", "--share", d, "--after-submit"])
            self.assertTrue((camp / f"scans/{doc}.jpg").exists())
            self.assertFalse(inc.exists())


@unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow required for scan decoder tests")
class DecodeQuarantineTest(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.state = self.root / "state" / "qwen-test"
        (self.root / "curated" / "pages").mkdir(parents=True)

    @staticmethod
    def image(fmt="JPEG", value=128):
        from PIL import Image

        output = io.BytesIO()
        Image.new("L", (64, 64), value).save(output, fmt)
        return output.getvalue()

    @staticmethod
    def page(seq, raw, ext="jpg"):
        doc = f"sn1_1900-01-02_ed-1_seq-{seq}"
        return {"doc_id": doc, "batch": "xx_test", "batch_version": 1, "lccn": "sn1",
                "date": "1900-01-02", "edition": 1, "seq": seq, "lang": "rus",
                "damage_rate": 0.8, "scan": f"buffer/xx_test/sn1/1900-01-02/1/{seq}.{ext}",
                "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}

    def texts(self):
        texts = mock.Mock()
        texts.loc.side_effect = lambda batch, version, docs: ({doc: "LoC text" for doc in docs}, True)
        texts.american_stories.return_value = {}
        texts.ours.return_value = {}
        texts.ours_alto.return_value = None
        return texts

    def args(self, pages):
        pages_file = self.root / "pages.jsonl"
        pages_file.write_text("".join(json.dumps(page) + "\n" for page in pages))
        return types.SimpleNamespace(campaign="qwen-test", curated=str(self.root / "curated"),
                                     state=str(self.root / "state"), outbox=str(self.root / "outbox"),
                                     pages=str(pages_file), scan_server="http://scan", chunk_pages=2,
                                     max_pending=4, workers=1, once=True, alto_cache=None,
                                     quarantine_decode=True)

    def test_real_truncated_jpeg_and_jp2_fail_decode_including_jpeg_passthrough(self):
        from PIL import features

        for fmt, is_jp2 in (("JPEG", False), ("JPEG2000", True)):
            if is_jp2 and not features.check_codec("jpg_2000"):
                continue
            with self.subTest(format=fmt):
                raw = self.image(fmt)
                jpg, size = feed.page_jpeg(raw, is_jp2)
                self.assertEqual(size, (64, 64))
                if not is_jp2:
                    self.assertEqual(jpg, raw)
                with self.assertRaises(feed.ScanDecodeError) as error:
                    feed.page_jpeg(raw[:-2], is_jp2)
                self.assertEqual(error.exception.original_type, "OSError")
                self.assertIn("truncated" if not is_jp2 else "broken data stream", str(error.exception))

    def test_missing_codec_and_indexed_format_disagreement_are_not_decode_quarantines(self):
        from PIL import features

        raw = self.image()
        with mock.patch.object(features, "check_codec", return_value=False), \
                self.assertRaisesRegex(RuntimeError, "decoder is unavailable"):
            feed.page_jpeg(raw, False)
        if features.check_codec("jpg_2000"):
            with self.assertRaisesRegex(ValueError, "format differs"):
                feed.page_jpeg(raw, True)
        with self.assertRaises(feed.ScanDecodeError) as error:
            feed.page_jpeg(b"not an image", False)
        self.assertEqual(error.exception.original_type, "UnidentifiedImageError")
        unknown = OSError("decoder resource unavailable")
        with self.assertRaises(OSError) as error:
            feed.raise_decode_error(unknown, "jpg")
        self.assertIs(error.exception, unknown)

    def test_hash_mismatch_is_retried_before_attempting_to_decode(self):
        page = self.page(1, self.image())
        self.state.mkdir(parents=True)
        camp = self.root / "campaign"
        camp.mkdir()
        args = types.SimpleNamespace(workers=1, scan_server="http://scan", alto_cache=None,
                                     quarantine_decode=True)
        with mock.patch.object(feed.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(b"bad")), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            for _ in range(3):
                self.assertEqual(feed.process_chunk(args, self.texts(), [page], str(camp), str(self.state), 1,
                                                    str(self.state / "done.txt"), str(self.state / "skipped.txt")),
                                 (0, 1, 1))
        self.assertIn("scan doesn't match its index line", output.getvalue())
        self.assertEqual(feed.scan_quarantine.quarantined(self.state), [])
        self.assertEqual(feed.scan_quarantine.retryable_ids(self.state), {feed.scan_quarantine.source_id(page)})

    def test_known_decode_failure_is_quarantined_after_three_runs_and_changed_bytes_can_prepare(self):
        from PIL import __version__ as pillow_version

        good_raw, bad_raw = self.image(value=40), self.image(value=128)[:-2]
        good, bad = self.page(1, good_raw), self.page(2, bad_raw)
        args = self.args([good, bad])
        scans = {good["scan"]: good_raw, bad["scan"]: bad_raw}
        calls = []

        def fetch(request, timeout):
            path = request.full_url.split("/scans/", 1)[1]
            calls.append(path)
            return io.BytesIO(scans[path])

        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=fetch), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            for attempt in range(2):
                with self.assertRaises(SystemExit) as error:
                    feed.cmd_run(args)
                self.assertEqual(error.exception.code, 1)
            feed.cmd_run(args)
            self.assertIn("scan quarantined; repair pending", output.getvalue())
            before = len(calls)
            feed.cmd_run(args)
            self.assertEqual(len(calls), before)
            replacement = self.image(value=200)
            scans[bad["scan"]] = replacement
            bad = self.page(2, replacement)
            Path(args.pages).write_text(json.dumps(bad) + "\n")
            feed.cmd_run(args)

        records = feed.scan_quarantine.quarantined(self.state)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["attempts"], 3)
        self.assertEqual(records[0]["refetch_state"], "pending")
        self.assertEqual(records[0]["source"]["sha256"], hashlib.sha256(bad_raw).hexdigest())
        self.assertEqual(records[0]["decoder"]["pillow"], pillow_version)
        self.assertFalse((self.state / "skipped.txt").exists())
        self.assertEqual(feed.read_lines(self.state / "done.txt"), {good["doc_id"], bad["doc_id"]})
        self.assertTrue((self.root / "outbox/qwen-test/c00001/READY").exists())
        self.assertTrue((self.root / "outbox/qwen-test/c00002/READY").exists())

    def test_network_integrity_and_unknown_decoder_errors_remain_retryable(self):
        page = self.page(1, self.image())
        self.state.mkdir(parents=True)
        camp = self.root / "campaign"
        camp.mkdir()
        args = types.SimpleNamespace(workers=1, scan_server="http://scan", alto_cache=None,
                                     quarantine_decode=True)
        for error in (urllib.error.URLError("timeout"), ValueError("scan doesn't match its index line"),
                      OSError("decoder resource unavailable"), RuntimeError("decoder is unavailable")):
            with self.subTest(error=type(error).__name__), \
                    mock.patch.object(feed, "prepare_page", side_effect=error), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                for _ in range(3):
                    self.assertEqual(feed.process_chunk(args, self.texts(), [page], str(camp), str(self.state), 1,
                                                        str(self.state / "done.txt"),
                                                        str(self.state / "skipped.txt")), (0, 1, 1))
                self.assertIn("page failed; will retry", output.getvalue())
        self.assertEqual(feed.scan_quarantine.quarantined(self.state), [])
        self.assertEqual(feed.scan_quarantine.retryable_ids(self.state), {feed.scan_quarantine.source_id(page)})
        with sqlite3.connect(self.state / feed.scan_quarantine.DB_NAME) as connection:
            self.assertEqual(connection.execute("SELECT attempts FROM failures").fetchone()[0], 0)
        self.assertFalse((self.state / "skipped.txt").exists())

    def test_a_successful_decode_resets_previous_attempts(self):
        page = self.page(1, self.image())
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        quarantine = feed.scan_quarantine
        self.assertEqual(quarantine.record_failure(self.state, page, error, error.decoder), 1)
        self.assertEqual(quarantine.record_failure(self.state, page, error, error.decoder), 2)
        quarantine.record_success(self.state, page)
        self.assertEqual(quarantine.record_failure(self.state, page, error, error.decoder), 1)
        self.assertEqual(quarantine.quarantined(self.state), [])
        quarantine.record_retryable(self.state, page, urllib.error.URLError("timeout"))
        quarantine.record_success(self.state, page)
        self.assertEqual(quarantine.retryable_ids(self.state), set())

    def test_transient_failure_does_not_increment_or_erase_decoder_failures(self):
        page = self.page(1, self.image())
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        quarantine = feed.scan_quarantine
        quarantine.record_failure(self.state, page, error, error.decoder)
        quarantine.record_failure(self.state, page, error, error.decoder)
        quarantine.record_retryable(self.state, page, urllib.error.URLError("timeout"))
        self.assertEqual(quarantine.retryable_ids(self.state), {quarantine.source_id(page)})
        self.assertEqual(quarantine.record_failure(self.state, page, error, error.decoder), 3)
        self.assertEqual(quarantine.retryable_ids(self.state), set())

    def test_concurrent_decoder_failures_are_counted_without_lost_updates(self):
        page = self.page(1, self.image()[:-2])
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        with ThreadPoolExecutor(max_workers=6) as executor:
            attempts = list(executor.map(
                lambda _: feed.scan_quarantine.record_failure(self.state, page, error, error.decoder), range(6)))
        self.assertEqual(sorted(attempts), list(range(1, 7)))
        self.assertEqual(feed.scan_quarantine.quarantined(self.state)[0]["attempts"], 6)

    def test_unreadable_or_corrupt_quarantine_state_is_not_silently_ignored(self):
        with mock.patch.object(Path, "stat", side_effect=PermissionError("state inaccessible")), \
                self.assertRaisesRegex(PermissionError, "state inaccessible"):
            feed.scan_quarantine.quarantined(self.state)
        self.state.mkdir(parents=True)
        (self.state / feed.scan_quarantine.DB_NAME).write_bytes(b"not a database")
        with self.assertRaises(sqlite3.DatabaseError):
            feed.scan_quarantine.quarantined(self.state)

    def test_inconsistent_retry_state_fails_closed(self):
        page = self.page(1, self.image())
        feed.scan_quarantine.record_retryable(self.state, page, urllib.error.URLError("timeout"))
        with sqlite3.connect(self.state / feed.scan_quarantine.DB_NAME) as connection:
            connection.execute("UPDATE failures SET refetch_state = 'queued'")
        with self.assertRaisesRegex(ValueError, "invalid scan-quarantine record"):
            feed.scan_quarantine.quarantined(self.state)
        with self.assertRaisesRegex(ValueError, "invalid scan-quarantine record"):
            feed.scan_quarantine.retryable_ids(self.state)

    def test_original_page_failure_is_logged_even_if_state_writing_fails(self):
        page = self.page(1, self.image())
        self.state.mkdir(parents=True)
        camp = self.root / "campaign"
        camp.mkdir()
        args = types.SimpleNamespace(workers=1, scan_server="http://scan", alto_cache=None,
                                     quarantine_decode=True)
        with mock.patch.object(feed, "prepare_page", side_effect=urllib.error.URLError("network down")), \
                mock.patch.object(feed.scan_quarantine, "record_retryable",
                                  side_effect=sqlite3.OperationalError("state read-only")), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                self.assertRaisesRegex(sqlite3.OperationalError, "state read-only"):
            feed.process_chunk(args, self.texts(), [page], str(camp), str(self.state), 1,
                               str(self.state / "done.txt"), str(self.state / "skipped.txt"))
        self.assertIn(page["doc_id"], output.getvalue())
        self.assertIn("network down", output.getvalue())

    def test_once_does_not_hide_earlier_retries_when_outbox_fills(self):
        bad_raw, good_raw = self.image()[:-2], self.image(value=40)
        pages = [self.page(1, bad_raw), self.page(2, good_raw), self.page(3, good_raw)]
        args = self.args(pages)
        args.chunk_pages, args.max_pending = 1, 1
        scans = {page["scan"]: raw for page, raw in zip(pages, (bad_raw, good_raw, good_raw))}
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed.urllib.request, "urlopen",
                                  side_effect=lambda request, timeout: io.BytesIO(
                                      scans[request.full_url.split("/scans/", 1)[1]])), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                self.assertRaises(SystemExit) as error:
            feed.cmd_run(args)
        self.assertEqual(error.exception.code, 1)
        self.assertIn("pages left to retry", output.getvalue())
        self.assertTrue((self.root / "outbox/qwen-test/c00001/READY").exists())

    def test_selection_excludes_only_the_quarantined_fingerprint(self):
        old = self.page(1, self.image()[:-2])
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        for _ in range(3):
            feed.scan_quarantine.record_failure(self.state, old, error, error.decoder)
        index, out = self.root / "index.jsonl", self.root / "selected.jsonl"
        index.write_text(json.dumps(old) + "\n")
        args = types.SimpleNamespace(index=str(index), staged=None, curated=str(self.root / "curated"),
                                     out=str(out), count=10, order="fetched", quarantine_state=str(self.state))
        with contextlib.redirect_stdout(io.StringIO()):
            feed.cmd_select(args)
            self.assertEqual(out.read_text(), "")
            new = self.page(1, self.image(value=10))
            index.write_text(json.dumps(old) + "\n" + json.dumps(new) + "\n")
            feed.cmd_select(args)
        self.assertEqual(json.loads(out.read_text())["sha256"], new["sha256"])


class FollowTest(unittest.TestCase):
    def test_a_busy_feeder_waits_only_until_the_shipper_takes_a_chunk(self):
        with tempfile.TemporaryDirectory() as d:
            for c in ("c00001", "c00002"):
                Path(d, c).mkdir()
                Path(d, c, "READY").write_text("200\n")
            slept = []

            def sleep(secs):
                slept.append(secs)
                if len(slept) == 3:
                    os.remove(os.path.join(d, "c00001", "READY"))

            with mock.patch.object(feed.time, "sleep", side_effect=sleep), \
                    mock.patch.object(feed.time, "monotonic", side_effect=lambda: 15 * len(slept)):
                feed.wait_for_room(d, 2, 600, busy=True)
            self.assertEqual(slept, [60, 15, 15])
            slept.clear()
            with mock.patch.object(feed.time, "sleep", side_effect=slept.append):
                feed.wait_for_room(d, 2, 600, busy=False)
            self.assertEqual(slept, [600])

    def test_a_full_outbox_still_waits_no_longer_than_every_secs(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "c00001").mkdir()
            Path(d, "c00001", "READY").write_text("200\n")
            clock = [0.0]

            def sleep(secs):
                clock[0] += secs

            with mock.patch.object(feed.time, "sleep", side_effect=sleep), \
                    mock.patch.object(feed.time, "monotonic", side_effect=lambda: clock[0]):
                feed.wait_for_room(d, 1, 120, busy=True)
            self.assertEqual(clock[0], 120)

    def test_page_of_names_the_set(self):
        row = {"doc_id": "sn1_1900-01-02_ed-1_seq-3", "batch": "b", "batch_version": 1, "bytes": 5,
               "sha256": "x", "format": "jp2", "fetched_at": "t"}
        p = feed.page_of(row, "buffer", {"sn1_1900-01-02_ed-1_seq-3": {"lang": "ger", "damage_rate": 0.4}})
        self.assertEqual(p["scan"], "buffer/b/sn1/1900-01-02/1/3.jp2")
        self.assertEqual((p["lang"], p["damage_rate"]), ("deu", 0.4))

    def test_only_shipped_quality_buffer_scans_are_deleted(self):
        with tempfile.TemporaryDirectory() as d:
            camp = Path(d, "camp")
            (camp / "c00002").mkdir(parents=True)  # still in the outbox
            done = Path(d, "done.txt")
            done.write_text("q1\tc00001\tbuffer/b/q1.jp2\n"   # shipped, quality, buffer: deleted
                            "o1\tc00001\tbuffer/b/o1.jp2\n"   # shipped, OCR's page: kept
                            "q2\tc00001\tkeep/b/q2.jp2\n"     # keep set: kept
                            "q3\tc00002\tbuffer/b/q3.jp2\n")  # not shipped yet: kept
            calls = []

            class Resp:
                def __enter__(self):
                    return self

                def __exit__(self, *a):
                    return False

            def fake(req, timeout=0):
                calls.append((req.get_method(), req.full_url))
                return Resp()

            a = type("A", (), {"scan_server": "http://s"})()
            with mock.patch.object(feed.urllib.request, "urlopen", fake):
                n = feed.delete_shipped(a, str(camp), str(done), {"q1", "q2", "q3"}, str(Path(d, "deleted.txt")))
            self.assertEqual(n, 1)
            self.assertEqual(calls, [("DELETE", "http://s/scans/buffer/b/q1.jp2")])
            with mock.patch.object(feed.urllib.request, "urlopen", fake):
                self.assertEqual(feed.delete_shipped(a, str(camp), str(done), {"q1"}, str(Path(d, "deleted.txt"))), 0)


class LoadTest(unittest.TestCase):
    def test_numbers_from_strings_and_nan(self):
        self.assertEqual(loader.num("0.25"), 0.25)
        self.assertIsNone(loader.num(float("nan")))
        self.assertIsNone(loader.num("none"))
        self.assertEqual(loader.integer("12.0"), 12)

    def test_upsert_sql_copies_then_inserts(self):
        sql = loader.upsert_sql("pages", ["doc_id", "lang"], ["doc_id"], [("a", None), ("b", "eng")])
        self.assertIn("COPY t_load (doc_id, lang) FROM STDIN WITH (FORMAT csv);\na,\nb,eng\n\\.\n", sql)
        self.assertIn("ON CONFLICT (doc_id) DO UPDATE SET lang = EXCLUDED.lang", sql)
        self.assertEqual(loader.upsert_sql("pages", ["doc_id"], ["doc_id"], []), "")

    def test_results_split_into_pages_and_readers(self):
        rows = [
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "blocks": 0, "est_cer": "0.2",
             "pairs_json": '{"as": {"cer_sym": 0.5}}', "job_id": "j", "outcome": "ok"},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "as", "blocks": 0, "est_cer": None,
             "pairs_json": None, "job_id": "j", "outcome": "ok"},
        ]
        units, scores = loader.result_rows(rows, "v1-test")
        self.assertEqual(len(units), 1)
        self.assertEqual(units[0][loader.UNIT_COLS.index("readers")], 2)
        self.assertFalse(units[0][loader.UNIT_COLS.index("has_alto")])
        self.assertEqual([s[2] for s in scores], ["loc-hist", "as"])
        self.assertEqual(scores[0][loader.SCORE_COLS.index("est_cer")], 0.2)

    def test_agreement_rows_map_loc_and_as_to_a_and_b(self):
        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "metric_version": 1,
             "cer_loc": 0.7, "cer_as": 0.6, "local_cer_sym": 0.05, "matched_loc": 0.9, "matched_as": 0.99,
             "loc_words": 100, "as_words": 90}
        rows = loader.agreement_rows([x, dict(x, local_cer_sym=0.06), {"doc_id": "bad"}], "text-agreement-v1")
        self.assertEqual(len(rows), 1)
        r = dict(zip(loader.AGREE_COLS, rows[0]))
        self.assertEqual((r["reader_a"], r["reader_b"]), ("loc-hist", "as"))
        self.assertEqual((r["cer_a"], r["cer_b"], r["local_cer_sym"]), (0.7, 0.6, 0.06))
        self.assertEqual((r["matched_a"], r["words_b"]), (0.9, 90))

    def test_hat_v2_tables_keep_both_versions_and_v1_tables_fill_v1(self):
        base = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "outcome": "ok", "job_id": "j",
                "blocks": 1, "pairs_json": None}
        v2 = dict(base, hat_version=2, est_cer="0.1", est_cer_v1="0.4", matched="0.7",
                  hat_determined=True, hat_determined_v1=False, hat_pairs_json='{"b": 1, "a": 2}')
        units, scores = loader.result_rows([v2], "v1-test")
        u = dict(zip(loader.UNIT_COLS, units[0]))
        r = dict(zip(loader.SCORE_COLS, scores[0]))
        self.assertEqual((u["hat_version"], u["hat_determined_v1"], u["hat_pairs"]), (2, False, '{"a": 2, "b": 1}'))
        self.assertEqual((r["est_cer"], r["est_cer_v1"], r["matched"]), (0.1, 0.4, 0.7))
        units, scores = loader.result_rows([dict(base, est_cer="0.3", hat_determined=True)], "v1-old")
        u = dict(zip(loader.UNIT_COLS, units[0]))
        r = dict(zip(loader.SCORE_COLS, scores[0]))
        self.assertEqual((u["hat_version"], u["hat_determined_v1"]), (1, True))
        self.assertEqual((r["est_cer"], r["est_cer_v1"], r["matched"]), (0.3, 0.3, None))

    def test_loc_word_confidence_rows(self):
        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 2, "words": 10,
             "words_with_wc": 9, "wc_mean": 0.81, "wc_median": 0.9, "wc_below": 0.1, "wc_threshold": 0.5,
             "software": "ABBYY", "archive": "b_ver02.tar.bz2"}
        rows = loader.locwc_rows([x, dict(x, wc_mean=0.7), {"doc_id": "nope", "batch": "b"}])
        self.assertEqual(len(rows), 1)
        r = dict(zip(loader.LOCWC_COLS, rows[0]))
        self.assertEqual((r["batch_version"], r["wc_mean"], r["words"]), (2, 0.7, 10))

    def test_damage_keeps_one_row_a_page(self):
        import datetime

        x = {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "lccn": "sn1",
             "date": datetime.date(1900, 1, 1), "edition": 1, "seq": 1, "lang": "eng", "damage_rate": 0.4}
        pages, damage = loader.damage_rows([x, dict(x, damage_rate=0.5)], "damage-v2")
        self.assertEqual(len(pages), 1)
        self.assertEqual(damage[0][loader.DAMAGE_COLS.index("damage_rate")], 0.5)


class ReceiveOwnerTest(unittest.TestCase):
    def test_a_file_already_the_owners_is_not_chowned(self):
        # Each chown on the share is a synchronous write; a chunk's walk meets thousands of files.
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "page.json")
            path.write_text("{}")
            info = os.lstat(path)
            with mock.patch.object(receive.os, "lchown") as chown:
                with mock.patch.object(receive, "OWNER", info.st_uid), \
                        mock.patch.object(receive.os, "lstat", return_value=mock.Mock(st_uid=info.st_uid, st_gid=info.st_uid)):
                    receive.own(str(path))
                chown.assert_not_called()
                with mock.patch.object(receive, "OWNER", info.st_uid + 1):
                    receive.own(str(path))
                chown.assert_called_once_with(str(path), info.st_uid + 1, info.st_uid + 1)


if __name__ == "__main__":
    unittest.main()
