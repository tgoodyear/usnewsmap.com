"""Tests for the on-prem quality feeder (#962), with no network or production state."""
import contextlib
from concurrent.futures import ThreadPoolExecutor
import hashlib
import importlib.util
import io
import json
import os
import sqlite3
import struct
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

    def follow_argv(self):
        return ["follow", "--campaign", "cpu-test", "--curated", str(self.root / "curated"),
                "--scan-server", "http://scan", "--outbox", str(self.root / "outbox"),
                "--state", str(self.root / "state"), "--staged-dsn", "test",
                "--chunk-pages", "1", "--max-pending", "1", "--workers", "1",
                "--once", "--quarantine-decode"]

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

    def test_real_jp2s_pass_the_completeness_check_and_flagged_cuts_do_not_decode(self):
        from PIL import Image, features

        if not features.check_codec("jpg_2000"):
            self.skipTest("Pillow's JPEG2000 codec is required")
        image = Image.effect_noise((96, 64), 40).convert("L")
        for options in ({}, {"tile_size": (32, 32)},
                        {"quality_mode": "rates", "quality_layers": [40, 10], "progression": "RLCP"}):
            output = io.BytesIO()
            image.save(output, "JPEG2000", **options)
            raw = output.getvalue()
            start, _ = feed.jp2_codestream(raw)
            # LoC's truncated scans have a 'jp2c' box that runs to the end of the file.
            to_end = raw[:start - 8] + struct.pack(">I", 0) + raw[start - 4:]
            with self.subTest(options=options):
                self.assertIsNone(feed.jp2_truncation(raw))
                self.assertIsNone(feed.jp2_truncation(to_end))
                for cut in sorted({1, 2, 3, 17, len(raw) // 4, len(raw) // 2, len(raw) - start}):
                    for short in (raw[:-cut], to_end[:-cut]):
                        self.assertIsNotNone(feed.jp2_truncation(short), cut)
                        with self.assertRaises(Exception):
                            feed.page_jpeg(short, True)

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
            self.assertIn("scan quarantined; operator review required", output.getvalue())
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
        self.assertEqual(records[0]["refetch_state"], "manual_review")
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

    def test_late_transient_failure_preserves_quarantine_evidence(self):
        page = self.page(1, self.image()[:-2])
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        for _ in range(3):
            feed.scan_quarantine.record_failure(self.state, page, error, error.decoder)
        before = feed.scan_quarantine.quarantined(self.state)[0]
        feed.scan_quarantine.record_retryable(self.state, page, urllib.error.URLError("timeout"))
        after = feed.scan_quarantine.quarantined(self.state)[0]
        self.assertEqual(after, before)

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

    def test_existing_recovery_state_is_not_silently_treated_as_manual_quarantine(self):
        page = self.page(1, self.image()[:-2])
        error = feed.ScanDecodeError(OSError("image file is truncated"), "jpg")
        for _ in range(3):
            feed.scan_quarantine.record_failure(self.state, page, error, error.decoder)
        with sqlite3.connect(self.state / feed.scan_quarantine.DB_NAME) as connection:
            connection.execute("UPDATE failures SET refetch_state = 'queued'")
        with self.assertRaisesRegex(ValueError, "invalid scan-quarantine record"):
            feed.scan_quarantine.quarantined(self.state)
        with sqlite3.connect(self.state / feed.scan_quarantine.DB_NAME) as connection:
            connection.execute("UPDATE failures SET refetch_state = 'manual_review', manifest_body = ?", (b"x",))
        with self.assertRaisesRegex(ValueError, "unexpected recovery data"):
            feed.scan_quarantine.quarantined(self.state)

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

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for the follower")
    def test_follow_skips_a_quarantined_fingerprint_before_filling_its_outbox(self):
        from PIL import features

        if not features.check_codec("jpg_2000"):
            self.skipTest("Pillow's JPEG2000 codec is required")
        # A cut-short JPEG: a cut-short JP2 is source_truncated before decoding (below).
        bad_raw, good_raw = self.image()[:-2], self.image("JPEG2000", value=40)
        bad, good = self.page(1, bad_raw), self.page(2, good_raw, "jp2")
        later = {page["doc_id"]: {**page, "format": fmt, "fetched_at": f"2026-11-03T03:0{i}:00Z"}
                 for i, (page, fmt) in enumerate(((bad, "jpeg"), (good, "jp2")))}
        # The good page arrives once the bad one is quarantined: before, it would stand in for it.
        rows = {bad["doc_id"]: later.pop(bad["doc_id"])}
        scans = {bad["scan"]: bad_raw, good["scan"]: good_raw}
        calls = []

        def fetch(request, timeout):
            path = request.full_url.split("/scans/", 1)[1]
            calls.append(path)
            return io.BytesIO(scans[path])

        state = self.root / "state/cpu-test"
        camp = self.root / "outbox/cpu-test"
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer") as scan_set, \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=fetch), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            for _ in range(3):
                feed.main(self.follow_argv())
            self.assertEqual(calls, [bad["scan"]] * 3)
            records = feed.scan_quarantine.quarantined(state)
            self.assertEqual((records[0]["attempts"], records[0]["refetch_state"]), (3, "manual_review"))
            self.assertIn("chunk came up short", output.getvalue())
            rows.update(later)
            feed.main(self.follow_argv())
            self.assertEqual(calls, [bad["scan"]] * 3 + [good["scan"]])
            self.assertEqual(scan_set.call_count, 4)
            self.assertEqual(feed.read_lines(state / "done.txt"), {good["doc_id"]})
            self.assertFalse((state / "skipped.txt").exists())
            self.assertIn("quarantined scans excluded", output.getvalue())

            (camp / "c00001/READY").unlink()
            replacement = self.image(value=200)
            scans[bad["scan"]] = replacement
            rows[bad["doc_id"]] = {**self.page(1, replacement), "format": "jpeg",
                                   "fetched_at": "2026-11-03T03:10:00Z"}
            feed.main(self.follow_argv())
            self.assertEqual(calls[-1], bad["scan"])
            self.assertEqual(feed.read_lines(state / "done.txt"), {bad["doc_id"], good["doc_id"]})
            self.assertTrue((camp / "c00002/READY").exists())

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for the follower")
    def test_follow_keeps_transient_errors_retryable(self):
        page = self.page(1, self.image())
        rows = {page["doc_id"]: {**page, "format": "jpeg", "fetched_at": "2026-11-03T03:00:00Z"}}
        state = self.root / "state/cpu-test"
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer"), \
                mock.patch.object(feed, "prepare_page", side_effect=urllib.error.URLError("timeout")) as prepare, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            for _ in range(4):
                feed.main(self.follow_argv())
        self.assertEqual(prepare.call_count, 4)
        self.assertIn("page failed; will retry", output.getvalue())
        self.assertEqual(feed.scan_quarantine.quarantined(state), [])
        self.assertEqual(feed.scan_quarantine.retryable_ids(state), {feed.scan_quarantine.source_id(page)})

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for the follower")
    def test_follow_without_quarantine_option_preserves_retry_behavior(self):
        raw = self.image()[:-2]
        page = self.page(1, raw)
        rows = {page["doc_id"]: {**page, "format": "jpeg", "fetched_at": "2026-11-03T03:00:00Z"}}
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer") as scan_set, \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(raw)), \
                contextlib.redirect_stdout(io.StringIO()):
            for _ in range(4):
                feed.main(self.follow_argv()[:-1])
        self.assertEqual(scan_set.call_count, 4)
        self.assertFalse((self.root / "state/cpu-test" / feed.scan_quarantine.DB_NAME).exists())


class ScanGoneTest(unittest.TestCase):
    """A buffer scan read and deleted by an OCR worker after selection (#1457); no Pillow needed."""

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.state = self.root / "state" / "qwen-test"
        (self.root / "curated" / "pages").mkdir(parents=True)

    @staticmethod
    def page(seq, store="buffer"):
        raw = f"scan {seq}".encode()
        return {"doc_id": f"sn1_1900-01-02_ed-1_seq-{seq}", "batch": "xx_test", "batch_version": 1,
                "lccn": "sn1", "date": "1900-01-02", "edition": 1, "seq": seq, "lang": "rus",
                "damage_rate": 0.8, "scan": f"{store}/xx_test/sn1/1900-01-02/1/{seq}.jpg",
                "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}, raw

    @staticmethod
    def texts():
        texts = mock.Mock()
        texts.loc.side_effect = lambda batch, version, docs: ({doc: "LoC text" for doc in docs}, True)
        texts.american_stories.return_value = {}
        texts.ours.return_value = {}
        texts.ours_alto.return_value = None
        return texts

    def test_a_buffer_scan_gone_since_selection_does_not_fail_the_run(self):
        (good, good_raw), (gone, _) = self.page(1), self.page(2)
        pages = self.root / "pages.jsonl"
        pages.write_text(json.dumps(good) + "\n" + json.dumps(gone) + "\n")
        args = types.SimpleNamespace(campaign="qwen-test", curated=str(self.root / "curated"),
                                     state=str(self.root / "state"), outbox=str(self.root / "outbox"),
                                     pages=str(pages), scan_server="http://scan", chunk_pages=2,
                                     max_pending=4, workers=1, once=True, alto_cache=None,
                                     quarantine_decode=True)

        def fetch(request, timeout):
            if request.full_url.endswith(gone["scan"]):
                raise urllib.error.HTTPError(request.full_url, 404, "Not Found", {}, None)
            return io.BytesIO(good_raw)

        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=fetch), \
                mock.patch.object(feed, "page_jpeg", return_value=(good_raw, (64, 64))), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            feed.cmd_run(args)
        self.assertIn("scan gone; left for a later selection", output.getvalue())
        self.assertNotIn("page failed; will retry", output.getvalue())
        self.assertEqual(feed.read_lines(self.state / "done.txt"), {good["doc_id"]})
        self.assertFalse((self.state / "skipped.txt").exists())
        self.assertTrue((self.root / "outbox/qwen-test/c00001/READY").exists())
        self.assertEqual(feed.scan_quarantine.quarantined(self.state), [])
        self.assertEqual(feed.scan_quarantine.retryable_ids(self.state), {feed.scan_quarantine.source_id(gone)})

    def test_a_missing_keep_scan_or_another_http_error_is_still_retried(self):
        self.state.mkdir(parents=True)
        camp = self.root / "campaign"
        camp.mkdir()
        args = types.SimpleNamespace(workers=1, scan_server="http://scan", alto_cache=None,
                                     quarantine_decode=True)
        for (page, _), code, retry in ((self.page(1), 404, 0), (self.page(1, "keep"), 404, 1),
                                       (self.page(1), 500, 1)):
            error = urllib.error.HTTPError("http://scan/scans/" + page["scan"], code, "error", {}, None)
            with self.subTest(scan=page["scan"], code=code), \
                    mock.patch.object(feed.urllib.request, "urlopen", side_effect=error), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(feed.process_chunk(args, self.texts(), [page], str(camp), str(self.state), 1,
                                                    str(self.state / "done.txt"),
                                                    str(self.state / "skipped.txt")), (0, retry, 1))
        self.assertFalse((self.state / "done.txt").exists())
        self.assertFalse((self.state / "skipped.txt").exists())


def codestream(tiles=1, data=b"\x80" * 20, last_runs_to_eoc=False):
    """A synthetic JPEG 2000 codestream: SOC, SIZ, COD, QCD, one tile-part a tile, EOC.
    The parser reads only marker and tile-part headers, so the entropy data is filler."""
    width = 32 * tiles
    main = (struct.pack(">HHH8IHBBB", 0xFF51, 41, 0, width, 32, 0, 0, 32, 32, 0, 0, 1, 7, 1, 1)
            + struct.pack(">HHBBHBBBBBB", 0xFF52, 12, 0, 0, 1, 0, 5, 4, 4, 0, 1)
            + struct.pack(">HHBB", 0xFF5C, 4, 0x40, 0x48))
    parts = b""
    for tile in range(tiles):
        psot = 0 if last_runs_to_eoc and tile == tiles - 1 else 14 + len(data)
        parts += struct.pack(">HHHIBB", 0xFF90, 10, tile, psot, 0, 1) + b"\xff\x93" + data
    return b"\xff\x4f" + main + parts + b"\xff\xd9"


def jp2(stream, lbox=None):
    """A JP2 file around a codestream; lbox=0 is a 'jp2c' box that runs to the end of the file."""
    ihdr = struct.pack(">I4sIIHBBBB", 22, b"ihdr", 32, 32, 1, 7, 7, 0, 0)
    colr = struct.pack(">I4sBBBI", 15, b"colr", 1, 0, 0, 17)
    header = (feed.JP2_SIGNATURE + struct.pack(">I4s4sI4s", 20, b"ftyp", b"jp2 ", 0, b"jp2 ")
              + struct.pack(">I4s", 8 + len(ihdr) + len(colr), b"jp2h") + ihdr + colr)
    return header + struct.pack(">I4s", 8 + len(stream) if lbox is None else lbox, b"jp2c") + stream


class SourceTruncatedTest(unittest.TestCase):
    """A JP2 cut short at LoC's source (#1457): flagged from its headers, never retried; no Pillow."""

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.state = self.root / "state" / "qwen-test"
        (self.root / "curated" / "pages").mkdir(parents=True)

    @staticmethod
    def page(seq, raw, batch="xx_test"):
        return {"doc_id": f"sn1_1900-01-02_ed-1_seq-{seq}", "batch": batch, "batch_version": 1,
                "lccn": "sn1", "date": "1900-01-02", "edition": 1, "seq": seq, "lang": "rus",
                "damage_rate": 0.8, "scan": f"buffer/{batch}/sn1/1900-01-02/1/{seq}.jp2",
                "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}

    def args(self, pages, quarantine_decode=True):
        pages_file = self.root / "pages.jsonl"
        pages_file.write_text("".join(json.dumps(page) + "\n" for page in pages))
        return types.SimpleNamespace(campaign="qwen-test", curated=str(self.root / "curated"),
                                     state=str(self.root / "state"), outbox=str(self.root / "outbox"),
                                     pages=str(pages_file), scan_server="http://scan", chunk_pages=4,
                                     max_pending=4, workers=1, once=True, alto_cache=None,
                                     quarantine_decode=quarantine_decode)

    @staticmethod
    def texts():
        texts = mock.Mock()
        texts.loc.side_effect = lambda batch, version, docs: ({doc: "LoC text" for doc in docs}, True)
        texts.american_stories.return_value = {}
        texts.ours.return_value = {}
        texts.ours_alto.return_value = None
        return texts

    @staticmethod
    def logged(output, msg):
        return [line for line in map(json.loads, output.getvalue().splitlines()) if line["msg"] == msg]

    def test_complete_or_otherwise_malformed_scans_are_left_to_the_decoder(self):
        four = codestream(4)
        xl = jp2(four)[:-len(four) - 8] + struct.pack(">I4sQ", 1, b"jp2c", 16 + len(four)) + four
        for name, raw in (("one tile", jp2(codestream())), ("four tiles", jp2(four)),
                          ("last tile-part runs to EOC", jp2(codestream(2, last_runs_to_eoc=True))),
                          ("jp2c box to end of file", jp2(four, lbox=0)), ("XL jp2c box", xl),
                          ("bare codestream", four), ("JPEG", b"\xff\xd8\xff\xe0" + b"\x00" * 40),
                          ("bad SOT length", jp2(four.replace(b"\xff\x90\x00\x0a", b"\xff\x90\x00\x0b", 1))),
                          ("not a codestream", jp2(b"\x00" * 40))):
            with self.subTest(name):
                self.assertIsNone(feed.jp2_truncation(raw))

    def test_cut_short_jp2s_are_flagged_with_where_they_end(self):
        four, whole = codestream(4), jp2(codestream(4))
        boundary = len(codestream(2)) - 2
        for name, raw, reason in (
                ("no EOC", jp2(four[:-2]), "the codestream ends without an EOC marker after 4 of 4 tiles"),
                ("box past the file", whole[:-10], "the 'jp2c' box declares 10 bytes past the end of the file"),
                ("inside a tile-part", jp2(four[:-10], lbox=0),
                 "tile-part 0 of tile 3 declares 8 bytes past the end of the codestream"),
                ("at a tile-part boundary", jp2(four[:boundary], lbox=0),
                 "the codestream ends without an EOC marker after 2 of 4 tiles"),
                ("inside a tile-part header", jp2(four[:boundary + 6], lbox=0),
                 "the codestream ends inside a tile-part header"),
                ("last tile-part with no EOC", jp2(codestream(2, last_runs_to_eoc=True)[:-2], lbox=0),
                 "tile 1's last tile-part runs to the end of the codestream with no EOC marker"),
                ("inside the main header", jp2(four[:20], lbox=0),
                 "main-header marker 0xFF51 runs past the end of the codestream"),
                ("only SOC", jp2(four[:2]), "the codestream ends inside its main header"),
                ("header boxes only", whole[:len(whole) - len(four) - 8],
                 "the file ends before its codestream box"),
                ("inside a header box", whole[:60], "the 'jp2h' box declares"),
                ("bare codestream", four[:-2], "the codestream ends without an EOC marker after 4 of 4 tiles")):
            with self.subTest(name):
                self.assertIn(reason, feed.jp2_truncation(raw) or "")

    def test_a_truncated_jp2_is_recorded_once_and_is_never_fetched_or_decoded_again(self):
        good_raw, bad_raw = jp2(codestream(2)), jp2(codestream(2)[:-2])
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
                mock.patch.object(feed, "page_jpeg", return_value=(b"jpeg", (32, 32))) as decode, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            feed.cmd_run(args)  # no SystemExit: nothing is left to retry
            self.assertEqual([call.args[0] for call in decode.call_args_list], [good_raw])
            self.assertEqual(self.logged(output, "scan truncated at source; not retried")[0]["doc_id"],
                             bad["doc_id"])
            self.assertEqual(self.logged(output, "source-truncated scans found"),
                             [{"msg": "source-truncated scans found", "pages": 1,
                               "batches": {"xx_test_ver01": 1}}])
            self.assertEqual(sorted(calls), sorted(scans))

            feed.cmd_run(args)
            self.assertEqual(len(calls), 2)
            self.assertEqual(self.logged(output, "source-truncated scans in campaign"),
                             [{"msg": "source-truncated scans in campaign", "campaign": "qwen-test",
                               "pages": 1, "batches": {"xx_test_ver01": 1}}])

            replacement = jp2(codestream(1))
            scans[bad["scan"]] = replacement
            Path(args.pages).write_text(json.dumps(self.page(2, replacement)) + "\n")
            feed.cmd_run(args)
            self.assertEqual(calls[-1], bad["scan"])

        records = feed.scan_quarantine.quarantined(self.state)
        self.assertEqual([(r["source"]["sha256"], r["refetch_state"], r["attempts"], r["error_type"])
                          for r in records],
                         [(bad["sha256"], "source_truncated", 0, "SourceTruncated")])
        self.assertIn("without an EOC marker after 2 of 2 tiles", records[0]["error"])
        self.assertEqual(feed.scan_quarantine.retryable_ids(self.state), set())
        self.assertEqual(feed.read_lines(self.state / "done.txt"), {good["doc_id"], bad["doc_id"]})
        self.assertFalse((self.state / "skipped.txt").exists())
        self.assertEqual(feed.scan_quarantine.quarantined_documents(self.root / "state"), {bad["doc_id"]})

    def test_without_quarantine_option_a_truncated_jp2_is_skipped_with_its_reason(self):
        raw = jp2(codestream(1)[:-2])
        page = self.page(1, raw)
        args = self.args([page], quarantine_decode=False)
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed.urllib.request, "urlopen",
                                  side_effect=lambda *a, **k: io.BytesIO(raw)) as fetch, \
                mock.patch.object(feed, "page_jpeg") as decode, \
                contextlib.redirect_stdout(io.StringIO()):
            feed.cmd_run(args)
            feed.cmd_run(args)
        self.assertEqual(fetch.call_count, 1)
        decode.assert_not_called()
        self.assertEqual((self.state / "skipped.txt").read_text(),
                         f"{page['doc_id']}\tsource_truncated: the codestream ends without an EOC marker "
                         "after 1 of 1 tiles\n")
        self.assertFalse((self.state / feed.scan_quarantine.DB_NAME).exists())

    def test_source_truncated_ends_observation_but_leaves_a_manual_review(self):
        quarantine = feed.scan_quarantine
        observed, reviewed, other = (self.page(1, b"one"), self.page(2, b"two"),
                                     self.page(3, b"three", batch="yy_test"))
        decoder = types.SimpleNamespace(original_type="OSError")
        quarantine.record_retryable(self.state, observed, OSError("timeout"))
        quarantine.record_failure(self.state, observed, decoder, {})
        for _ in range(3):
            quarantine.record_failure(self.state, reviewed, decoder, {})
        for page in (observed, reviewed, other, other):
            quarantine.record_source_truncated(self.state, page, "the codestream ends early")
        records = {r["source"]["doc_id"]: r for r in quarantine.quarantined(self.state)}
        self.assertEqual({doc: (r["refetch_state"], r["attempts"]) for doc, r in records.items()},
                         {observed["doc_id"]: ("source_truncated", 1),
                          reviewed["doc_id"]: ("manual_review", 3),
                          other["doc_id"]: ("source_truncated", 0)})
        self.assertEqual(quarantine.retryable_ids(self.state), set())
        quarantine.record_success(self.state, observed)
        self.assertEqual(len(quarantine.quarantined(self.state)), 3)
        self.assertEqual(quarantine.source_truncated_summary(quarantine.quarantined(self.state)),
                         {"pages": 2, "batches": {"xx_test_ver01": 1, "yy_test_ver01": 1}})

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for the follower")
    def test_follow_records_a_truncated_jp2_and_excludes_it_from_later_rounds(self):
        good_raw, bad_raw = jp2(codestream(1)), jp2(codestream(1)[:-10], lbox=0)
        good, bad = self.page(1, good_raw), self.page(2, bad_raw)
        rows = {page["doc_id"]: {**page, "format": "jp2", "fetched_at": f"2026-11-03T03:0{i}:00Z"}
                for i, page in enumerate((bad, good))}
        scans = {bad["scan"]: bad_raw, good["scan"]: good_raw}
        calls = []

        def fetch(request, timeout):
            path = request.full_url.split("/scans/", 1)[1]
            calls.append(path)
            return io.BytesIO(scans[path])

        argv = ["follow", "--campaign", "cpu-test", "--curated", str(self.root / "curated"),
                "--scan-server", "http://scan", "--outbox", str(self.root / "outbox"),
                "--state", str(self.root / "state"), "--staged-dsn", "test", "--chunk-pages", "2",
                "--max-pending", "1", "--workers", "1", "--once", "--quarantine-decode", "--max-wait-secs", "0"]
        state = self.root / "state/cpu-test"
        with mock.patch.object(feed, "Texts", return_value=self.texts()), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer"), \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=fetch), \
                mock.patch.object(feed, "page_jpeg", return_value=(b"jpeg", (32, 32))), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            feed.main(argv)
            self.assertEqual(calls, [bad["scan"], good["scan"]])
            (self.root / "outbox/cpu-test/c00001/READY").unlink()
            feed.main(argv)
        self.assertEqual(len(calls), 2)
        self.assertEqual(self.logged(output, "quarantined scans excluded")[0]["pages"], 1)
        self.assertEqual(self.logged(output, "source-truncated scans in campaign")[0]["batches"],
                         {"xx_test_ver01": 1})
        self.assertEqual(feed.scan_quarantine.quarantined(state)[0]["error"],
                         "tile-part 0 of tile 0 declares 8 bytes past the end of the codestream")
        self.assertEqual(feed.read_lines(state / "done.txt"), {good["doc_id"]})


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

    def test_most_damaged_shipped_scans_are_kept_within_the_budget(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("q1\tc1\tbuffer/b/q1.jp2\t40\t0.9\n"
                            "q2\tc1\tbuffer/b/q2.jp2\t40\t0.5\n"
                            "q3\tc1\tbuffer/b/q3.jp2\t40\t0.1\n"
                            "q4\tc1\tbuffer/b/q4.jp2\t10\t\n"      # unscored: last, after q3 ends the list
                            "q5\tc1\tbuffer/b/q5.jp2\n")           # written before sizes: deleted
            calls, gone = [], set()

            class Resp(io.BytesIO):
                pass

            def fake(req, timeout=0):
                path = req.full_url.removeprefix("http://s/scans/")
                calls.append((req.get_method(), path))
                if path.removesuffix(".json") in gone:
                    raise urllib.error.HTTPError(req.full_url, 404, "Not Found", {}, None)
                return Resp(b"{}")

            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=85, keep_verify=10)
            ship = lambda docs, ocr=(): feed.delete_shipped(a, str(camp), str(done), docs, str(deleted), str(kept),
                                                            set(ocr))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ship({"q1", "q2", "q3", "q4", "q5"}), 3)
                self.assertEqual(calls, [("DELETE", f"buffer/b/{q}.jp2") for q in ("q3", "q4", "q5")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"q1", "q2"})

                # An empty quality-docs list (missing, say) releases nothing.
                self.assertEqual(ship(set()), 0)
                self.assertEqual(set(feed.read_kept(str(kept))), {"q1", "q2"})

                # OCR asks for q2 and q1's sidecar is found gone: q2 stays listed and counted,
                # unlooked-up, while q1 leaves once a DELETE makes sure of its bytes, which makes
                # room for a newly shipped q6.
                calls.clear()
                gone.add("buffer/b/q1.jp2")
                with open(done, "a") as f:
                    f.write("q6\tc2\tbuffer/b/q6.jp2\t40\t0.3\n")
                self.assertEqual(ship({"q1", "q3", "q4", "q5", "q6"}, {"q2"}), 0)
                self.assertEqual(calls, [("GET", "buffer/b/q1.jp2.json"), ("DELETE", "buffer/b/q1.jp2")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"q2", "q6"})
                self.assertEqual(feed.read_kept(str(kept))["q2"][3], 0)
                self.assertEqual(feed.read_lines(str(deleted)), {"q1", "q3", "q4", "q5"})

                # OCR is done with q2, but its worker's best-effort DELETE failed: the lookup
                # after finds it there, so it stays a kept scan like any other.
                calls.clear()
                self.assertEqual(ship({"q6"}), 0)
                self.assertEqual(calls, [("GET", "buffer/b/q2.jp2.json"), ("GET", "buffer/b/q6.jp2.json")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"q2", "q6"})

                # No budget at all evicts every one.
                calls.clear()
                a.keep_bytes = 0
                self.assertEqual(ship({"q6"}), 2)
                self.assertEqual(calls, [("DELETE", "buffer/b/q2.jp2"), ("DELETE", "buffer/b/q6.jp2")])
                self.assertEqual(feed.read_kept(str(kept)), {})

    def test_a_kept_scan_ocr_asks_for_stays_counted_until_a_lookup_after_finds_its_bytes_gone(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("new\tc1\tbuffer/b/new.jp2\t40\t0.9\n")
            # ocr was checked later than other ever will be: only OCR asking for it puts it first.
            kept.write_text("ocr\tbuffer/b/ocr.jp2\t40\t0.1\t4102444800\nother\tbuffer/b/other.jp2\t10\t0.95\t1\n")
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=60, keep_verify=1)
            calls, status = [], {}

            def fake(req, timeout=0):
                path = req.full_url.removeprefix("http://s/scans/")
                calls.append((req.get_method(), path))
                if status.get(path):
                    raise urllib.error.HTTPError(req.full_url, status[path], "error", {}, None)
                return io.BytesIO(b"{}")

            ship = lambda ocr=(): feed.delete_shipped(a, str(camp), str(done), {"new"}, str(deleted), str(kept),
                                                      set(ocr))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                # While OCR wants it, ocr isn't looked up or evicted and still counts: new doesn't
                # fit beside it, though it's more damaged.
                self.assertEqual(ship({"ocr"}), 1)
                self.assertEqual(calls, [("GET", "buffer/b/other.jp2.json"), ("DELETE", "buffer/b/new.jp2")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"ocr", "other"})

                # Once OCR is done with it, it's looked up first. OCR's worker DELETE got only as
                # far as its sidecar, so it stays until one of ours makes sure of its bytes.
                calls.clear()
                status.update({"buffer/b/ocr.jp2.json": 404, "buffer/b/ocr.jp2": 500})
                self.assertEqual(ship(), 0)
                self.assertEqual(calls, [("GET", "buffer/b/ocr.jp2.json"), ("DELETE", "buffer/b/ocr.jp2")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"ocr", "other"})

                calls.clear()
                a.keep_verify = 2
                status["buffer/b/ocr.jp2"] = 0
                self.assertEqual(ship(), 0)
                self.assertEqual(sorted(calls), [("DELETE", "buffer/b/ocr.jp2"), ("GET", "buffer/b/ocr.jp2.json"),
                                                 ("GET", "buffer/b/other.jp2.json")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"other"})
                self.assertEqual(feed.read_lines(str(deleted)), {"new", "ocr"})

    def test_a_kept_scan_that_fails_to_go_holds_its_place_and_its_replacement_is_deleted(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("old\tc1\tbuffer/b/old.jp2\t80\t0.2\n")
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=80, keep_verify=0)
            fail = {"buffer/b/old.jp2"}
            calls = []

            def fake(req, timeout=0):
                path = req.full_url.removeprefix("http://s/scans/")
                calls.append((req.get_method(), path))
                if path in fail:
                    raise urllib.error.HTTPError(req.full_url, 503, "Unavailable", {}, None)
                return io.BytesIO(b"")

            ship = lambda: feed.delete_shipped(a, str(camp), str(done), {"old", "new"}, str(deleted), str(kept))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ship(), 0)
                with open(done, "a") as f:
                    f.write("new\tc2\tbuffer/b/new.jp2\t80\t0.9\n")
                self.assertEqual(ship(), 1)  # old's DELETE fails: it stays, and new goes instead
                self.assertEqual(set(feed.read_kept(str(kept))), {"old"})
                self.assertEqual(calls[-2:], [("DELETE", "buffer/b/old.jp2"), ("DELETE", "buffer/b/new.jp2")])
                self.assertEqual(feed.read_lines(str(deleted)), {"new"})

    def test_a_failed_kept_scan_check_goes_to_the_back_and_only_no_answer_ends_the_round(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("")
            kept.write_text("".join(f"{doc}\tbuffer/b/{doc}.jp2\t10\t0.5\t{n}\n"
                                    for n, doc in enumerate(["down", "flaky", "gone", "ok"], 1)))
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=100, keep_verify=3)
            calls, up = [], [False]

            def fake(req, timeout=0):
                doc = req.full_url.split("/")[-1].split(".")[0]
                calls.append(doc)
                if not up[0]:
                    raise urllib.error.URLError("connection refused")
                if doc in ("flaky", "gone"):
                    raise urllib.error.HTTPError(req.full_url, 503 if doc == "flaky" else 404, "", {}, None)
                return io.BytesIO(b"{}")

            ship = lambda: feed.delete_shipped(a, str(camp), str(done), {"down", "flaky", "gone", "ok"},
                                               str(deleted), str(kept))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                ship()
                self.assertEqual(calls, ["down"])
                up[0] = True
                calls.clear()
                ship()
            self.assertEqual(calls, ["flaky", "gone", "gone", "ok"])  # gone: its check, then its DELETE
            self.assertEqual(set(feed.read_kept(str(kept))), {"down", "flaky", "ok"})

    def test_a_kept_scan_whose_sidecar_is_gone_but_bytes_are_not_stays_until_deleted(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("")
            kept.write_text("half\tbuffer/b/half.jp2\t10\t0.9\t1\n")
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=100, keep_verify=1)
            unlink = [None]

            def fake(req, timeout=0):
                if req.get_method() == "GET":  # the sidecar: renamed by a DELETE whose unlink failed
                    raise urllib.error.HTTPError(req.full_url, 404, "", {}, None)
                if unlink[0] == "reset":
                    raise urllib.error.URLError(ConnectionResetError("reset by peer"))
                if unlink[0] != 204:
                    raise urllib.error.HTTPError(req.full_url, unlink[0], "", {}, None)
                return io.BytesIO(b"")

            ship = lambda: feed.delete_shipped(a, str(camp), str(done), {"half"}, str(deleted), str(kept))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                for fails in (500, "reset"):
                    unlink[0] = fails
                    ship()
                    self.assertEqual(set(feed.read_kept(str(kept))), {"half"})
                    self.assertGreater(feed.read_kept(str(kept))["half"][3], 1)  # checked: to the back
                    self.assertEqual(feed.read_lines(str(deleted)), set())
                unlink[0] = 204
                ship()
            self.assertEqual(feed.read_kept(str(kept)), {})
            self.assertEqual(feed.read_lines(str(deleted)), {"half"})

    def test_a_scan_server_that_stops_answering_mid_round_loses_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("new\tc2\tbuffer/b/new.jp2\t80\t0.9\n")
            kept.write_text("old\tbuffer/b/old.jp2\t80\t0.2\t1\n")
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=80, keep_verify=1)
            calls, down = [], [True]

            def fake(req, timeout=0):
                calls.append((req.get_method(), req.full_url.removeprefix("http://s/scans/")))
                if down[0] and req.get_method() == "DELETE":
                    raise urllib.error.URLError(ConnectionResetError("reset by peer"))
                return io.BytesIO(b"{}")

            ship = lambda: feed.delete_shipped(a, str(camp), str(done), {"new"}, str(deleted), str(kept))
            with mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ship(), 0)
                # old's eviction got no answer: new's DELETE isn't tried, and old's check is kept.
                self.assertEqual(calls, [("GET", "buffer/b/old.jp2.json"), ("DELETE", "buffer/b/old.jp2")])
                self.assertEqual(set(feed.read_kept(str(kept))), {"old"})
                self.assertGreater(feed.read_kept(str(kept))["old"][3], 1)
                self.assertEqual(feed.read_lines(str(deleted)), set())
                down[0] = False
                self.assertEqual(ship(), 1)
            self.assertEqual(set(feed.read_kept(str(kept))), {"new"})
            self.assertEqual(feed.read_lines(str(deleted)), {"old"})

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "PyArrow required for the follower")
    def test_a_new_campaign_cleans_up_after_the_last_and_looks_after_the_kept_list_from_the_start(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "curated/pages").mkdir(parents=True)
            (root / "state/old").mkdir(parents=True)
            (root / "outbox/old/c00002").mkdir(parents=True)  # not shipped yet: left alone
            (root / "state/followed.txt").write_text("old\n")
            # old kept k1, which OCR's worker has since read; q1 and q3 shipped after old's
            # follower last ran, and q2 is still in the outbox.
            (root / "state/old/done.txt").write_text("k1\tc00001\tbuffer/b/k1.jp2\t40\t0.9\n"
                                                     "q1\tc00001\tbuffer/b/q1.jp2\t40\t0.5\n"
                                                     "q2\tc00002\tbuffer/b/q2.jp2\t40\t0.3\n"
                                                     "q3\tc00001\tbuffer/b/q3.jp2\t40\t0.1\n")
            (root / "state/kept.tsv").write_text("k1\tbuffer/b/k1.jp2\t40\t0.9\t0\n")
            (root / "quality-docs.txt").write_text("k1\nq1\nq2\nq3\n")
            (root / "ocr-docs.txt").write_text("")
            (root / "ocr-active.txt").write_text("")
            calls = []

            def fake(req, timeout=0):
                path = req.full_url.removeprefix("http://scan/scans/")
                calls.append((req.get_method(), path))
                if path == "buffer/b/k1.jp2.json":
                    raise urllib.error.HTTPError(req.full_url, 404, "Not Found", {}, None)
                return io.BytesIO(b"{}")

            argv = ["follow", "--campaign", "new", "--curated", str(root / "curated"), "--scan-server", "http://scan",
                    "--outbox", str(root / "outbox"), "--state", str(root / "state"), "--staged-dsn", "test",
                    "--quality-docs", str(root / "quality-docs.txt"), "--ocr-docs", str(root / "ocr-docs.txt"),
                    "--ocr-active", str(root / "ocr-active.txt"), "--keep-bytes", "60", "--once"]
            with mock.patch.object(feed, "staged_from_db", return_value=set()), \
                    mock.patch.object(feed, "index_rows", return_value={}), \
                    mock.patch.object(feed.urllib.request, "urlopen", fake), \
                    contextlib.redirect_stdout(io.StringIO()):
                # Before new has shipped anything, k1 is found gone, and the room it leaves
                # goes to old's q1 (q3 doesn't fit). k1 isn't taken back from old's done.txt.
                feed.main(argv)
                self.assertEqual(calls, [("GET", "buffer/b/k1.jp2.json"), ("DELETE", "buffer/b/k1.jp2"),
                                         ("DELETE", "buffer/b/q3.jp2")])
                self.assertEqual(set(feed.read_kept(str(root / "state/kept.tsv"))), {"q1"})
                self.assertEqual(feed.read_lines(root / "state/retired.txt"), {"k1"})
                self.assertEqual(feed.read_lines(root / "state/old/deleted.txt"), {"q3"})
                self.assertEqual(feed.read_lines(root / "state/followed.txt"), {"old", "new"})

                calls.clear()
                feed.main(argv)
                self.assertEqual(calls, [("GET", "buffer/b/q1.jp2.json")])
                self.assertEqual(set(feed.read_kept(str(root / "state/kept.tsv"))), {"q1"})

                # While OCR wants q1 it isn't looked up: by ocr-active.txt, or by ocr-docs.txt
                # until there is one. Once OCR's outcome ends it, it is, though ocr-docs.txt
                # still lists it.
                (root / "ocr-docs.txt").write_text("q1\n")
                (root / "ocr-active.txt").unlink()
                calls.clear()
                feed.main(argv)
                self.assertEqual(calls, [])
                (root / "ocr-active.txt").write_text("q1\n")
                feed.main(argv)
                self.assertEqual(calls, [])
                (root / "ocr-active.txt").write_text("")
                feed.main(argv)
                self.assertEqual(calls, [("GET", "buffer/b/q1.jp2.json")])
                self.assertEqual(set(feed.read_kept(str(root / "state/kept.tsv"))), {"q1"})

                # q2 ships once OCR wants it, though ocr-docs.txt doesn't say so yet: it's OCR's
                # scan, neither kept nor deleted.
                (root / "outbox/old/c00002").rmdir()
                (root / "ocr-active.txt").write_text("q2\n")
                calls.clear()
                feed.main(argv)
                self.assertEqual(calls, [("GET", "buffer/b/q1.jp2.json")])
                self.assertEqual(set(feed.read_kept(str(root / "state/kept.tsv"))), {"q1"})

    def test_an_unreadable_kept_line_is_weighed_again_not_fatal(self):
        with tempfile.TemporaryDirectory() as d:
            camp, done, deleted, kept = Path(d, "camp"), Path(d, "done.txt"), Path(d, "deleted.txt"), Path(d, "kept.tsv")
            camp.mkdir()
            done.write_text("x\tc1\tbuffer/b/x.jp2\t80\t0.5\n")
            kept.write_text("x\tbuffer/b/x.jp2\t8")  # cut off mid-write
            a = types.SimpleNamespace(scan_server="http://s", keep_bytes=100, keep_verify=0)
            with mock.patch.object(feed.urllib.request, "urlopen") as urlopen, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(feed.delete_shipped(a, str(camp), str(done), {"x"}, str(deleted), str(kept)), 0)
            urlopen.assert_not_called()
            self.assertEqual(feed.read_kept(str(kept))["x"][:3], ("buffer/b/x.jp2", 80, 0.5))


class FollowChunkTest(unittest.TestCase):
    """follow writes full chunks, and a part one only once --max-wait-secs has passed."""

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        (self.root / "curated" / "pages").mkdir(parents=True)

    @unittest.skipUnless(importlib.util.find_spec("pyarrow") and importlib.util.find_spec("PIL"),
                         "PyArrow and Pillow required for the follower")
    def test_a_part_chunk_waits_for_more_pages_until_overdue(self):
        raw = DecodeQuarantineTest.image()
        pages = [DecodeQuarantineTest.page(seq, raw) for seq in (1, 2, 3)]
        rows = {p["doc_id"]: {**p, "format": "jpeg", "fetched_at": f"2026-11-03T03:0{p['seq']}:00Z"}
                for p in pages}
        argv = ["follow", "--campaign", "cpu-test", "--curated", str(self.root / "curated"),
                "--scan-server", "http://scan", "--outbox", str(self.root / "outbox"),
                "--state", str(self.root / "state"), "--staged-dsn", "test", "--chunk-pages", "2",
                "--max-pending", "4", "--workers", "1", "--max-wait-secs", "3600", "--once"]
        state, camp = self.root / "state/cpu-test", self.root / "outbox/cpu-test"
        import pyarrow as pa
        import pyarrow.parquet as pq

        (self.root / "curated/audit/damage/v2").mkdir(parents=True)
        pq.write_table(pa.table({"doc_id": [pages[0]["doc_id"]], "lang": ["rus"], "damage_rate": [0.8]}),
                       self.root / "curated/audit/damage/v2/xx_test_ver01.parquet")
        with mock.patch.object(feed, "Texts", return_value=DecodeQuarantineTest.texts(self)), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer") as scan_set, \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(raw)), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            # A fresh state's clock starts now: the full chunk goes and the remainder waits.
            feed.main(argv)
            self.assertEqual((camp / "c00001/READY").read_text().strip(), "2")
            self.assertFalse((camp / "c00002").exists())
            done = [line.split("\t") for line in (state / "done.txt").read_text().splitlines()]
            self.assertEqual([r[0] for r in done], [pages[0]["doc_id"], pages[1]["doc_id"]])
            self.assertEqual(done[0][1:], ["c00001", pages[0]["scan"], str(len(raw)), "0.8"])
            self.assertEqual(done[1][4], "")  # unscored

            # A chunk was just written: the last page waits, without looking at the scan server.
            checked = scan_set.call_count
            feed.main(argv)
            self.assertEqual(scan_set.call_count, checked)
            self.assertFalse((camp / "c00002").exists())
            self.assertIn("waiting for a full chunk", output.getvalue())

            old = feed.time.time() - 3600
            os.utime(state / "chunks.txt", (old, old))
            feed.main(argv)
            self.assertEqual((camp / "c00002/READY").read_text().strip(), "1")
            self.assertEqual(feed.read_lines(state / "done.txt"), set(rows))

    @unittest.skipUnless(importlib.util.find_spec("pyarrow") and importlib.util.find_spec("PIL"),
                         "PyArrow and Pillow required for the follower")
    def test_a_page_that_fails_is_replaced_and_a_chunk_still_short_waits(self):
        raw = DecodeQuarantineTest.image()
        pages = [DecodeQuarantineTest.page(seq, raw) for seq in (1, 2, 3)]
        later = {p["doc_id"]: {**p, "format": "jpeg", "fetched_at": f"2026-11-03T03:0{p['seq']}:00Z"}
                 for p in pages}
        rows = {d: later.pop(d) for d in (pages[0]["doc_id"], pages[1]["doc_id"])}
        argv = ["follow", "--campaign", "cpu-test", "--curated", str(self.root / "curated"),
                "--scan-server", "http://scan", "--outbox", str(self.root / "outbox"),
                "--state", str(self.root / "state"), "--staged-dsn", "test", "--chunk-pages", "2",
                "--max-pending", "4", "--workers", "1", "--max-wait-secs", "3600", "--once"]
        state, camp = self.root / "state/cpu-test", self.root / "outbox/cpu-test"
        fetched = []

        def fetch(request, timeout):
            fetched.append(request.full_url.rsplit("/", 1)[1])
            if request.full_url.endswith("/" + pages[0]["scan"]):
                raise ConnectionResetError("reset")
            return io.BytesIO(raw)

        with mock.patch.object(feed, "Texts", return_value=DecodeQuarantineTest.texts(self)), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer"), \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=fetch), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            # Two pages make a full chunk, but one can't be fetched: one page isn't a unit yet.
            feed.main(argv)
            self.assertFalse((camp / "c00001").exists())
            self.assertFalse((state / "done.txt").exists())
            self.assertIn("chunk came up short", output.getvalue())

            # A third page stands in for the one that fails.
            rows.update(later)
            feed.main(argv)
            self.assertEqual((camp / "c00001/READY").read_text().strip(), "2")
            self.assertEqual(feed.read_lines(state / "done.txt"), {pages[1]["doc_id"], pages[2]["doc_id"]})
            self.assertEqual(fetched, [pages[0]["scan"].rsplit("/", 1)[1], pages[1]["scan"].rsplit("/", 1)[1]] * 2
                             + [pages[2]["scan"].rsplit("/", 1)[1]])

    @unittest.skipUnless(importlib.util.find_spec("pyarrow") and importlib.util.find_spec("PIL"),
                         "PyArrow and Pillow required for the follower")
    def test_pages_waiting_for_curated_text_do_not_make_a_chunk_full(self):
        raw = DecodeQuarantineTest.image()
        pages = [DecodeQuarantineTest.page(seq, raw) for seq in (1, 2, 3)]
        rows = {p["doc_id"]: {**p, "format": "jpeg", "fetched_at": f"2026-11-03T03:0{p['seq']}:00Z"}
                for p in pages}
        rows[pages[0]["doc_id"]]["batch"] = "xx_later"
        argv = ["follow", "--campaign", "cpu-test", "--curated", str(self.root / "curated"),
                "--scan-server", "http://scan", "--outbox", str(self.root / "outbox"),
                "--state", str(self.root / "state"), "--staged-dsn", "test", "--chunk-pages", "3",
                "--max-pending", "4", "--workers", "1", "--max-wait-secs", "3600", "--once"]
        texts = DecodeQuarantineTest.texts(self)
        texts.has_loc.side_effect = lambda batch, bv: batch != "xx_later"
        with mock.patch.object(feed, "Texts", return_value=texts), \
                mock.patch.object(feed, "staged_from_db", return_value=set()), \
                mock.patch.object(feed, "index_rows", return_value=rows), \
                mock.patch.object(feed, "scan_set", return_value="buffer") as scan_set, \
                mock.patch.object(feed.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(raw)), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            feed.main(argv)
        self.assertFalse((self.root / "outbox/cpu-test/c00001").exists())
        self.assertEqual(scan_set.call_count, 0)
        self.assertIn("waiting for the curated copy's LoC text", output.getvalue())
        self.assertIn('"pages": 2', output.getvalue().split("waiting for a full chunk")[1][:200])

    def test_a_batch_without_curated_text_is_looked_for_again_and_one_with_is_remembered(self):
        texts = feed.Texts(str(self.root / "curated"))
        self.assertFalse(texts.has_loc("xx_later", 1))
        run = self.root / "curated/pages/xx_later/v01/run1"
        run.mkdir(parents=True)
        (run / "part-0.parquet").write_bytes(b"")
        with mock.patch.object(feed.glob, "glob", wraps=feed.glob.glob) as looked:
            self.assertTrue(texts.has_loc("xx_later", 1))
            self.assertTrue(texts.has_loc("xx_later", 1))
        self.assertEqual(looked.call_count, 1)

    def test_a_fresh_state_is_not_overdue(self):
        state = self.root / "state"
        state.mkdir()
        self.assertLess(feed.time.time() - feed.last_chunk_at(str(state)), 60)
        self.assertEqual(feed.read_lines(state / "chunks.txt"), set())


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
