"""Guards on what the quality wave runner may change: python3 -m unittest -v scripts/boinc/test_quality_corpus_runner.py

The runner shares the BOINC project with every other app, so each database write must be
scoped to usnm_quality, and it must not touch GPU admission (plan_class_spec.xml) at all.
"""

import importlib.util
import hashlib
import json
import os
import re
import sys
import tempfile
import types
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

RUNNER_PATH = Path(__file__).resolve().parent / "quality_corpus_runner.py"
SOURCE = RUNNER_PATH.read_text()


def load_runner():
    # Importing runs nothing: main() is behind __main__, duckdb and Pillow import lazily.
    spec = importlib.util.spec_from_file_location("quality_corpus_runner", RUNNER_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Scope(unittest.TestCase):
    def test_no_gpu_admission_or_other_apps_versions(self):
        self.assertNotIn("plan_class_spec", SOURCE)
        self.assertNotIn("min_wu_id", SOURCE)
        self.assertNotIn("plan_class LIKE", SOURCE)

    def test_every_update_is_scoped_to_quality(self):
        updates = re.findall(r"UPDATE\s+\w+.*?;", SOURCE, flags=re.S)
        self.assertGreaterEqual(len(updates), 10)
        for stmt in updates:
            with self.subTest(stmt=" ".join(stmt.split())[:120]):
                # Named quality units or app versions, or a unit name / id that the runner took
                # from a usnm_quality query ({wu}, {wid}).
                self.assertTrue(any(k in stmt for k in ("usnm_quality", "'{wu}'", "{wid}")), stmt)
        self.assertNotRegex(SOURCE, r"UPDATE host SET")

    def test_ids_it_cancels_come_from_quality_units(self):
        block = SOURCE.split("cancelled_missing_dl = 0")[1].split("{wid}")[0]
        self.assertIn("w.name LIKE 'usnm_quality_%'", block)

    def test_units_are_replaced_only_on_positive_evidence(self):
        self.assertIn("not in_mysql or mysql_wus[wu][1] != 0", SOURCE)

    def test_a_failed_lookup_is_not_an_empty_database(self):
        lookup = SOURCE.split("def query_mysql_workunits")[1].split("\ndef ")[0]
        self.assertIn("raise", lookup)
        self.assertIn('"skipped": f"workunit lookup failed: {e}"', SOURCE)

    def test_an_existing_healthy_unit_keeps_its_bundle(self):
        self.assertIn("or (in_mysql and mysql_wus[wu][1] == 0)", SOURCE)

    def test_a_failed_submission_never_renames_existing_work(self):
        # The only rename is the repair step's, for a unit the transitioner marked failed.
        self.assertEqual(SOURCE.count("rename_colliding_workunit(wu)") - SOURCE.count("def rename_colliding_workunit(wu)"), 1)
        submit = SOURCE.split("submit.lock")[1].split("transitioner -d 2 --one_pass")[0]
        self.assertNotIn("rename_colliding_workunit", submit)
        self.assertIn("existing and existing[1] == 0", submit)


class SharedPlaces(unittest.TestCase):
    def test_recovery_copies_legacy_input_without_replacing_either_existing_file(self):
        for existing_v2 in (False, True):
            with self.subTest(existing_v2=existing_v2), tempfile.TemporaryDirectory() as tmp:
                r = load_runner()
                root = Path(tmp)
                checksum = hashlib.sha256(b"places").hexdigest()
                job = {"inputs": {"places": {"sha256": checksum}}}
                legacy = root / f"usnm_quality_places_{checksum}"
                current = root / f"usnm_quality_places_v2_{checksum}"
                legacy_data = b"old cache" if existing_v2 else b"places"
                legacy.write_bytes(legacy_data)
                if existing_v2:
                    current.write_bytes(b"places")
                with mock.patch.object(r, "dir_hier_path", lambda name: str(root / name)):
                    physical = r.recover_quality_places(job)
                self.assertEqual(root / physical, current)
                self.assertEqual(legacy.read_bytes(), legacy_data)
                self.assertEqual(current.read_bytes(), b"places")

    def test_recovery_does_not_copy_a_corrupt_legacy_input(self):
        r = load_runner()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            checksum = hashlib.sha256(b"places").hexdigest()
            job = {"inputs": {"places": {"sha256": checksum}}}
            legacy = root / f"usnm_quality_places_{checksum}"
            legacy.write_bytes(b"corrupt")
            with mock.patch.object(r, "dir_hier_path", lambda name: str(root / name)):
                with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                    r.recover_quality_places(job)
            self.assertFalse((root / f"usnm_quality_places_v2_{checksum}").exists())
            self.assertEqual(legacy.read_bytes(), b"corrupt")

    def test_fresh_bundle_submits_the_isolated_shared_input(self):
        r = load_runner()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            stage = root / "stage"
            bundle = stage / "units" / "bundle-0"
            bundle.mkdir(parents=True)
            units, locks = root / "units", root / "locks"
            units.mkdir()
            locks.mkdir()
            checksum = hashlib.sha256(b"places").hexdigest()
            job = {
                "id": "a" * 64,
                "inputs": {
                    name: {"bytes": len(data), "sha256": checksum}
                    for name, data in (("page", b"page"), ("alto", b"alto"),
                                       ("texts", b"texts"), ("places", b"places"))
                },
            }
            for name, data in (("page.jpg", b"page"), ("loc.xml", b"alto"),
                               ("texts.json", b"texts"), ("places.txt", b"places")):
                (bundle / name).write_bytes(data)
            (bundle / "quality.json").write_text(json.dumps(job))
            with mock.patch.multiple(r, PROJ=str(root), UNITS_DIR=str(units), LOCAL_LOCKS_DIR=str(locks)), \
                 mock.patch.object(r, "dir_hier_path", lambda name: str(root / "download" / name)), \
                 mock.patch.object(r, "wait_for_queue_headroom"), \
                 mock.patch.object(r, "collect_and_purge"), \
                 mock.patch.object(r, "to_sample_row", return_value={}), \
                 mock.patch.object(r.subprocess, "run", return_value=types.SimpleNamespace(returncode=0)) as run:
                submitted = r.flush_bundle(0, [("doc", {})], 1, str(stage),
                                           str(root / "scans"), str(root / "texts"), {"bundle_names": []})
            create = next(call.args[0] for call in run.call_args_list
                          if call.args[0][0].endswith("/bin/create_work"))
            physical = f"usnm_quality_places_v2_{checksum}"
            self.assertEqual(submitted, 1)
            self.assertIn(physical, create)
            self.assertNotIn(f"usnm_quality_places_{checksum}", create)
            self.assertEqual((root / "download" / physical).read_bytes(), b"places")
            self.assertEqual(len(list(units.glob("*/submitted"))), 1)


class NoPasswordOnCommandLines(unittest.TestCase):
    # A failed mysql call's traceback prints its command, and job logs keep it: on 20 Oct 2026
    # the runtime password reached Log Analytics that way. It goes in a 0600 options file.
    def test_mysql_reads_credentials_from_a_private_options_file(self):
        body = SOURCE.split("def run_mysql(sql):")[1].split("\ndef ")[0]
        self.assertIn("--defaults-extra-file=", body)
        self.assertNotRegex(body, r"-p\{|-u\{|-h\{")
        self.assertIn("tempfile.mkstemp(", SOURCE.split("def mysql_defaults_file():")[1].split("\ndef ")[0])

    def test_concurrent_writers_publish_one_private_file(self):
        # The quality job runs eight runners; their first calls can overlap.
        import multiprocessing, os, stat, tempfile
        body = "def mysql_defaults_file():" + SOURCE.split("def mysql_defaults_file():")[1].split("\ndef ")[0]
        with tempfile.TemporaryDirectory() as d:
            Path(d, "db-runtime").write_text("pw\"x\n")
            source = (body.replace("/dev/shm/boinc/db-runtime", f"{d}/db-runtime")
                      .replace("Path(MYSQL_DEFAULTS)", f"Path({d!r}) / 'mysql-runtime.cnf'"))
            ctx = multiprocessing.get_context("fork")
            procs = [ctx.Process(target=_write_defaults, args=(source,)) for _ in range(16)]
            for proc in procs:
                proc.start()
            for proc in procs:
                proc.join()
            self.assertTrue(all(proc.exitcode == 0 for proc in procs))
            path = Path(d, "mysql-runtime.cnf")
            self.assertEqual(path.read_text(), '[client]\nhost="db.example"\nuser="usnmboincrun"\npassword="pw\\"x"\n')
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(sorted(os.listdir(d)), ["db-runtime", "mysql-runtime.cnf"])

    def test_no_secret_on_any_command_line(self):
        self.assertNotRegex(SOURCE, r'f"-p\{')


class NoSas(unittest.TestCase):
    # ADR-0009: no SAS of any kind. Every blob call signs in with the job's managed identity (#916).
    def test_no_sas_variable_or_parameter(self):
        self.assertNotRegex(SOURCE, r"\b[A-Z]+_SAS\b")
        self.assertNotRegex(SOURCE, r"def list_blobs\([^)]*\bsas\b")

    def test_no_sas_query_passthrough(self):
        self.assertNotIn("sv=", SOURCE)
        self.assertNotIn("sig=", SOURCE)


class LocFallback(unittest.TestCase):
    def test_off_unless_asked_for(self):
        # One 5-second pacer for every shard can't fetch a corpus (#914): the LoC pass runs
        # only with QUALITY_LOC_FALLBACK=1, and every pass loop goes through PASSES.
        self.assertIn('LOC_FALLBACK = os.environ.get("QUALITY_LOC_FALLBACK", "0") == "1"', SOURCE)
        self.assertIn('PASSES = ("jpeg_cached", "jpeg_cached", "archived") + (("loc_fallback",) if LOC_FALLBACK else ())', SOURCE)
        self.assertNotRegex(SOURCE, r'for pass_mode in \("')


class MergeCache(unittest.TestCase):
    # Units are marked collected before pages.parquet is merged: a merge or upload that fails
    # must leave the next call rebuilding, though newly_collected is then 0 (#906).
    def setUp(self):
        self.r = load_runner()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)
        (self.d / "collected").mkdir()
        (self.d / "meta").mkdir()
        self.cache = self.d / "last_merged_stats.json"

    def part(self, name, data=b"x"):
        p = self.d / "collected" / name
        p.write_bytes(data)
        return str(p)

    def published(self, parts, metas=()):
        inv = self.r.merge_inventory(parts, list(metas))
        self.cache.write_text(json.dumps({"inventory_sha256": inv, "totals": {"total_rows": 1}}))

    def test_a_part_left_out_of_the_last_merge_forces_a_rebuild(self):
        a = self.part("part-1-0000-0020.parquet")
        self.published([a])
        b = self.part("part-2-0000-0020.parquet")  # collected; then the merge or upload failed
        stats, current = self.r.load_merged_stats(self.cache, self.r.merge_inventory([a, b], []))
        self.assertFalse(current)
        self.assertEqual(stats["totals"], {"total_rows": 1})

    def test_the_same_inputs_are_not_merged_again(self):
        a = self.part("part-1-0000-0020.parquet")
        self.published([a])
        _, current = self.r.load_merged_stats(self.cache, self.r.merge_inventory([a], []))
        self.assertTrue(current)

    def test_a_missing_unreadable_or_old_cache_forces_a_rebuild(self):
        inv = self.r.merge_inventory([self.part("part-1-0000-0020.parquet")], [])
        self.assertEqual(self.r.load_merged_stats(self.cache, inv), ({}, False))
        self.cache.write_text("{not json")
        self.assertEqual(self.r.load_merged_stats(self.cache, inv), ({}, False))
        self.cache.write_text(json.dumps({"totals": {}}))  # written before #906: no inventory
        self.assertFalse(self.r.load_merged_stats(self.cache, inv)[1])

    def test_rewritten_batch_metadata_republishes(self):
        a = self.part("part-1-0000-0020.parquet")
        meta = self.d / "meta" / "b_ver01.parquet"
        meta.write_bytes(b"partial")
        self.published([a], [str(meta)])
        meta.write_bytes(b"whole batch")
        os.utime(meta, ns=(1, 1))
        _, current = self.r.load_merged_stats(self.cache, self.r.merge_inventory([a], [str(meta)]))
        self.assertFalse(current)

    def test_collect_publishes_by_inventory_and_records_it_after_the_upload(self):
        body = SOURCE.split("def collect_and_purge(")[1].split("\ndef ")[0]
        self.assertIn("if part_files and not merge_current:", body)
        self.assertNotIn("newly_collected > 0 or", body)
        upload = body.index('put_blob("audit/quality/v1-wave9/pages.parquet"')
        self.assertLess(upload, body.index('merged_stats["inventory_sha256"] = inventory'))


class FakeCon:
    """Enough of a duckdb connection for the runner's loaders, with no duckdb installed."""

    def __init__(self, loc_rows=(), loc_error=None, damage_rows=()):
        self.loc_rows, self.loc_error, self.damage_rows = list(loc_rows), loc_error, list(damage_rows)
        self.inserted = []
        self.copied_to = []

    def execute(self, sql, params=None):
        rows = []
        if "ocr_source, text" in sql:
            if self.loc_error:
                raise self.loc_error
            rows = self.loc_rows
        elif "tmp_dmg.parquet" in sql:
            rows = self.damage_rows
        elif sql.startswith("COPY _bmeta TO"):
            dst = sql.split("'")[1]
            Path(dst).write_text(json.dumps(self.inserted))
            self.copied_to.append(dst)
        elif "CREATE OR REPLACE TEMP TABLE _bmeta" in sql:
            self.inserted = []
        return types.SimpleNamespace(fetchall=lambda: rows)

    def executemany(self, sql, rows):
        self.inserted.extend(rows)


class FakeImage:
    format, size = "JPEG2000", (10, 20)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def convert(self, mode):
        return self

    def save(self, path, *args, **kwargs):
        Path(path).write_bytes(b"jpeg")

    def close(self):
        pass


def damage_row(doc_id, batch="b", bv=1, date="1900-01-01"):
    return (doc_id, batch, bv, "sn1", date, 1, 1, "eng", "detected", 0.1)


class BatchMetadata(unittest.TestCase):
    # A batch outside the manifest staged over several passes must have batch metadata for
    # every page, or collection publishes the later passes' pages as batch 'unknown' (#906).
    def setUp(self):
        self.r = load_runner()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)
        self.r.STAGED_META_DIR = str(self.d / "meta")
        os.makedirs(self.r.STAGED_META_DIR)
        self.work = self.d / "work"
        self.work.mkdir()
        docs = ["sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2", "sn1_1900-01-01_ed-1_seq-3"]
        self.docs = docs
        self.keep = {f"keep/b/sn1/1900-01-01/1/{i}.jp2": d for i, d in enumerate(docs, 1)}
        self.con = FakeCon(damage_rows=[damage_row(d) for d in docs])

    def load(self, already, pass_mode="archived"):
        def list_blobs(*args, delimiter=None):  # prefix is last, with or without a sas argument
            return list(self.keep) if args[-1] == "keep/b/" else []

        def download_blob(url, dst, retries=4):
            Path(dst).write_bytes(b"pq")
            return True

        with mock.patch.object(self.r, "list_blobs", list_blobs), \
                mock.patch.object(self.r, "download_blob", download_blob):
            return self.r.load_batch_records(
                self.con, "b", 1, 1, str(self.work), set(already), pass_mode=pass_mode)

    def staged_docs(self):
        meta = Path(self.r.STAGED_META_DIR) / "b_ver01.parquet"
        return sorted(row[0] for row in json.loads(meta.read_text()))

    def test_metadata_covers_the_whole_batch_whichever_pages_a_pass_stages(self):
        recs = self.load(already={self.docs[0]})
        self.assertEqual([x["doc_id"] for x in recs], self.docs[1:])
        self.assertEqual(self.staged_docs(), self.docs)
        self.assertTrue(self.r.batch_meta_complete("b", 1))

    def test_a_partial_file_from_an_earlier_pass_is_rebuilt(self):
        meta = Path(self.r.STAGED_META_DIR) / "b_ver01.parquet"
        meta.write_text(json.dumps([[self.docs[0], "b", 1, "1900-01-01"]]))  # no .complete marker
        self.load(already={self.docs[0]})
        self.assertEqual(self.staged_docs(), self.docs)

    def test_a_fully_bundled_batch_with_partial_metadata_is_repaired(self):
        meta = Path(self.r.STAGED_META_DIR) / "b_ver01.parquet"
        meta.write_text(json.dumps([[self.docs[0], "b", 1, "1900-01-01"]]))
        self.assertEqual(self.load(already=set(self.docs)), [])
        self.assertEqual(self.staged_docs(), self.docs)
        # Repaired once; after that the batch is skipped without reading its damage file.
        self.con.damage_rows = []
        with mock.patch.object(self.r, "write_batch_meta") as write:
            self.assertEqual(self.load(already=set(self.docs)), [])
            write.assert_not_called()

    def test_a_failed_archive_listing_leaves_the_loc_pass_batch_for_later(self):
        listings = {"n": 0}

        def list_blobs(*args, delimiter=None):
            if args[-1] == "keep/b/":
                listings["n"] += 1
                if listings["n"] == 1:
                    raise OSError("listing timed out")
                return list(self.keep)
            return []

        def download_blob(url, dst, retries=4):
            Path(dst).write_bytes(b"pq")
            return True

        def load():
            with mock.patch.object(self.r, "list_blobs", list_blobs), \
                    mock.patch.object(self.r, "download_blob", download_blob):
                return self.r.load_batch_records(
                    self.con, "b", 1, 1, str(self.work), set(), pass_mode="loc_fallback")

        self.assertEqual(load(), [])  # not every page as unarchived, so none reaches the LoC lookup
        recs = load()
        self.assertEqual([x["doc_id"] for x in recs], self.docs)
        self.assertTrue(all(x["blob_name"] for x in recs))

    def test_complete_metadata_is_not_rewritten(self):
        self.load(already=set())
        self.con.copied_to.clear()
        self.load(already={self.docs[0]})
        self.assertEqual(self.con.copied_to, [])


class SkipList(unittest.TestCase):
    # Only confirmed permanent exclusions go on the list later waves skip; a failed download
    # or Parquet read is retried by the next pass or wave (#906).
    def setUp(self):
        self.r = load_runner()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        d = Path(self.tmp.name)
        for name in ("UNITS_DIR", "LOCKS_DIR", "LOCAL_LOCKS_DIR"):
            os.makedirs(d / name)
            setattr(self.r, name, str(d / name))
        self.r.EXCLUDED_DOCS_PATH = str(d / "excluded_docs.txt")
        self.r.NUM_SHARDS = 1
        self.r.PASSES = ("archived", "archived")
        self.docs = ["sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2", "sn1_1900-01-01_ed-1_seq-3"]
        self.flushed = []
        self.requested = []

    def run_shard(self, con, loc_download_ok=lambda n: True, scan_fail=lambda did, n: False,
                  blob_name=lambda did: f"keep/b/{did}.jp2"):
        r = self.r
        calls = {"loc": 0, "scan": {}}

        def load_batch_records(con_, batch, bv, tier, work_dir, already_docs, **kw):
            recs = [{"doc_id": d, "batch": "b", "batch_version": 1, "lccn": "sn1", "date": "1900-01-01",
                     "edition": 1, "seq": 1, "blob_name": blob_name(d)} for d in self.docs if d not in already_docs]
            self.requested.append([x["doc_id"] for x in recs])
            return recs

        def list_blobs(*args, delimiter=None):
            return ["pages/b/v01/0.parquet"] if args[-1] == "pages/b/v01/" else []

        def download_blob(url, dst, retries=4):
            if "/pages/b/v01/" in url:
                calls["loc"] += 1
                return loc_download_ok(calls["loc"])
            return False

        def download_bytes(url, retries=4):
            did = url.split("/keep/b/")[1].split(".jp2")[0]
            calls["scan"][did] = calls["scan"].get(did, 0) + 1
            if scan_fail(did, calls["scan"][did]):
                raise urllib.error.URLError("connection reset")
            return b"jp2"

        def flush_bundle(shard_idx, chunk, *args):
            self.flushed.extend(d for d, _ in chunk)
            return 1

        pil = types.ModuleType("PIL")
        pil.Image = types.SimpleNamespace(open=lambda *a, **k: FakeImage(), MAX_IMAGE_PIXELS=None)
        duck = types.ModuleType("duckdb")
        duck.connect = lambda: con
        with mock.patch.dict(sys.modules, {"PIL": pil, "duckdb": duck}), \
                mock.patch.object(r, "discover_all_corpus_batches", lambda c: [("b", 1, 3, 1)]), \
                mock.patch.object(r, "load_batch_records", load_batch_records), \
                mock.patch.object(r, "list_blobs", list_blobs), \
                mock.patch.object(r, "download_blob", download_blob), \
                mock.patch.object(r, "download_bytes", download_bytes), \
                mock.patch.object(r, "cached_page", lambda b: None), \
                mock.patch.object(r, "flush_bundle", flush_bundle), \
                mock.patch.object(r, "put_status", lambda *a: None), \
                mock.patch.object(r.LocBatchResolver, "resolve", lambda self, *a: (None, None)):
            r.process_shard(0)

    def excluded(self):
        p = Path(self.r.EXCLUDED_DOCS_PATH)
        return sorted(p.read_text().split()) if p.exists() else []

    def loc_rows(self, docs):
        return [(d, "ndnp-original", "text") for d in docs]

    def test_missing_ocr_after_a_failed_text_download_is_retried(self):
        con = FakeCon(loc_rows=self.loc_rows(self.docs[:2]))
        self.run_shard(con, loc_download_ok=lambda n: n > 1)
        self.assertEqual(self.excluded(), [self.docs[2]])  # pass 2 read the texts in full
        self.assertEqual(self.requested, [self.docs, self.docs])  # pass 1 staged nothing
        self.assertEqual(self.flushed, self.docs[:2])

    def test_missing_ocr_after_a_failed_parquet_read_is_not_excluded(self):
        con = FakeCon(loc_error=RuntimeError("truncated parquet"))
        self.run_shard(con)
        self.assertEqual(self.excluded(), [])
        self.assertEqual(self.requested, [self.docs, self.docs])

    def test_missing_ocr_in_a_batch_read_in_full_is_excluded(self):
        self.run_shard(FakeCon(loc_rows=self.loc_rows(self.docs[:2])))
        self.assertEqual(self.excluded(), [self.docs[2]])

    def test_a_failed_page_download_is_retried_not_excluded(self):
        con = FakeCon(loc_rows=self.loc_rows(self.docs))
        self.run_shard(con, scan_fail=lambda did, n: did == self.docs[1] and n == 1)
        self.assertEqual(self.excluded(), [])
        self.assertEqual(self.requested[1], [self.docs[1]])  # pass 2 tries it again
        self.assertEqual(sorted(self.flushed), self.docs)

    def test_a_page_with_no_source_is_excluded(self):
        con = FakeCon(loc_rows=self.loc_rows(self.docs))
        self.run_shard(con, blob_name=lambda did: "" if did == self.docs[0] else f"keep/b/{did}.jp2")
        self.assertEqual(self.excluded(), [self.docs[0]])
        self.assertEqual(sorted(self.flushed), self.docs[1:])

    def test_a_failed_manifest_download_is_retried_not_read_as_no_entry(self):
        r = self.r
        r.BATCH_INDEX_DIR = self.tmp.name
        resolver = r.LocBatchResolver("b", 1)
        mets = (b'<mets xmlns="http://www.loc.gov/METS/" xmlns:xlink="http://www.w3.org/1999/xlink">'
                b'<dmdSec ID="d1"><start>1</start></dmdSec>'
                b'<fileSec><file ID="f1" USE="service"><FLocat xlink:href="./0001.jp2"/></file></fileSec>'
                b'<structMap><div TYPE="np:page" DMDID="d1"><fptr FILEID="f1"/></div></structMap></mets>')
        replies = [urllib.error.URLError("connection reset"),
                   b"abc *data/sn1/reel1/1900010101/1900010101.xml\n", mets]

        def loc_paced_download(url):
            reply = replies.pop(0)
            if isinstance(reply, Exception):
                raise reply
            return reply

        with mock.patch.object(r, "loc_paced_download", loc_paced_download):
            with self.assertRaises(urllib.error.URLError):
                resolver.resolve("sn1", "1900-01-01", 1, 1)
            jp2, _ = resolver.resolve("sn1", "1900-01-01", 1, 1)
        self.assertTrue(jp2.endswith("data/sn1/reel1/1900010101/0001.jp2"), jp2)

    def test_the_old_mixed_list_is_not_read(self):
        self.assertNotIn("skipped_docs.txt\"", SOURCE.replace("skipped_docs.txt, which", ""))
        self.assertIn("skipped_path = Path(EXCLUDED_DOCS_PATH)", SOURCE)


def _write_defaults(source):
    import contextlib, os, tempfile
    os.environ["DB_HOST"] = "db.example"
    os.environ.pop("DB_RUNTIME_USER", None)
    scope = {"Path": Path, "os": os, "tempfile": tempfile, "contextlib": contextlib}
    exec(source, scope)
    scope["mysql_defaults_file"]()


if __name__ == "__main__":
    unittest.main()
