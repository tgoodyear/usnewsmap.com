"""Tests for the worst-first campaign builder (#1415): no database, no network, no pyarrow needed."""
import collections
import contextlib
import csv
import datetime as dt
import hashlib
import importlib.util
import io
import json
import random
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_campaign", HERE / "quality_campaign.py")
qc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qc)
SELECT_KEYS = {"batch", "batch_version", "bytes", "damage_rate", "date", "doc_id", "edition", "fetched_at",
               "lang", "lccn", "scan", "seq", "sha256"}


def row(lccn, day, seq, rate, band=None, batch="ak_a", lang="eng", in_campaign="f", ed=1, year=1900, version=1,
        has_ocr="f"):
    date = f"{year}-01-{day:02d}"
    return [f"{lccn}_{date}_ed-{ed}_seq-{seq}", batch, version, lccn, date, year // 10 * 10, lang, "detected",
            rate, band or ("worst" if rate > 0.25 else "moderate"), in_campaign, has_ocr]


def fixture():
    rows = []
    for i in range(30):
        rows.append(row("sn1", 1 + i % 3, i + 1, 0.9 - i * 0.02, batch="ak_a" if i % 2 else "ak_b"))
    for i in range(300):
        lccn = f"sn{100 + i % 7}"
        rows.append(row(lccn, 1 + i % 9, i + 1, 0.11 + (i % 14) / 100, batch="ca_c", lang="ger" if i % 3 else "eng",
                        year=1890 + 10 * (i % 2)))
    return rows


def write_csv(path, rows):
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(qc.COLUMNS)
        w.writerows(rows)


def run(args, seed="s"):
    out = io.StringIO()
    args = ["--seed", seed, *args]
    with contextlib.redirect_stdout(out), mock.patch.object(qc.ocr_manifest, "manifest_bytes", fake_manifest):
        qc.main(args)
    return json.loads(out.getvalue())


def fake_manifest(rows):
    rows = sorted(rows, key=lambda r: (r["batch"], r["lccn"], r["date"], r["edition"], r["seq"], r["doc_id"]))
    return json.dumps(rows, default=str).encode()


def lines(path):
    with open(path) as f:
        return [json.loads(x) for x in f]


class Built(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)
        self.state = self.d / "ocr-state"
        self.state.mkdir()

    def build(self, rows, *extra, name="out", seed="s", cell="20"):
        src = self.d / f"{name}.csv"
        write_csv(src, rows)
        return run(self.args(src, name) + ["--cell-pages", cell, *extra], seed=seed)

    def args(self, src, name="out"):
        return ["--campaign", "w", "--input", str(src), "--out-dir", str(self.d / name), "--ocr-state", str(self.state)]


class OrderTest(Built):
    def test_worst_tiers_stay_in_order_and_batches_group_inside_them(self):
        s = self.build(fixture(), "--tier-width", "0.1")
        plan = lines(self.d / "out/w/plan.jsonl")
        self.assertEqual(s["worst"]["pages"], 30)
        self.assertEqual([p["rank"] for p in plan], list(range(1, 31)))
        tiers = [p["tier"] for p in plan]
        self.assertEqual(tiers, sorted(tiers))
        for early in range(len(plan)):
            for late in range(early, len(plan)):
                self.assertLess(plan[late]["damage_rate"] - plan[early]["damage_rate"], 0.1 + 1e-9)
        for tier in set(tiers):
            batches = [p["batch"] for p in plan if p["tier"] == tier]
            self.assertLessEqual(sum(1 for a, b in zip(batches, batches[1:]) if a != b), 1)

    def test_the_worst_batch_of_a_tier_goes_first_and_pages_in_issue_order(self):
        rows = [row("sn2", 2, 1, 0.31, batch="aa_low"), row("sn3", 2, 2, 0.34, batch="zz_high"),
                row("sn3", 1, 3, 0.33, batch="zz_high")]
        self.build(rows, "--tier-width", "0.05")
        plan = lines(self.d / "out/w/plan.jsonl")
        self.assertEqual([p["doc_id"][:16] for p in plan], ["sn3_1900-01-01_e", "sn3_1900-01-02_e", "sn2_1900-01-02_e"])

    def test_seq_sorts_as_a_number(self):
        self.build([row("sn2", 1, 10, 0.3), row("sn2", 1, 9, 0.3)])
        self.assertEqual([p["seq"] for p in lines(self.d / "out/w/plan.jsonl")], [9, 10])


class SampleTest(Built):
    def test_the_sample_is_apart_from_the_worst_and_capped_per_cell(self):
        s = self.build(fixture())
        worst = {p["doc_id"] for p in lines(self.d / "out/w/plan.jsonl")}
        sample = lines(self.d / "out/w-moderate/plan.jsonl")
        self.assertFalse(worst & {p["doc_id"] for p in sample})
        self.assertTrue(all(p["band"] == "moderate" for p in sample))
        cells = collections.Counter((p["page_lang"], p["decade"]) for p in sample)
        self.assertEqual(s["moderate"]["cells"], 4)
        self.assertTrue(all(n == 20 for n in cells.values()), cells)

    def test_titles_take_turns(self):
        self.build(fixture())
        sample = lines(self.d / "out/w-moderate/plan.jsonl")
        per = collections.Counter((p["page_lang"], p["decade"], p["lccn"]) for p in sample)
        cells = collections.defaultdict(list)
        for key, n in per.items():
            cells[key[:2]].append(n)
        for counts in cells.values():
            self.assertLessEqual(max(counts) - min(counts), 1)

    def test_a_small_cell_gives_all_it_has(self):
        rows = [row("sn5", 1, i, 0.2, lang="pol") for i in range(1, 6)]
        s = self.build(rows)
        self.assertEqual(s["moderate"]["pages"], 5)
        self.assertEqual(s["moderate"]["weights_sum"], 5)

    def test_weights_sum_to_the_cell_even_when_titles_outnumber_the_picks(self):
        rows = [row(f"sn{i}", 1, 1, 0.2) for i in range(1, 40)] + [row("sn1", 1, 2, 0.2), row("sn1", 1, 3, 0.2)]
        s = self.build(rows, cell="5")
        self.assertEqual(s["moderate"]["pages"], 5)
        self.assertAlmostEqual(s["moderate"]["weights_sum"], len(rows), places=3)
        self.assertEqual(s["moderate"]["most_pages_from_a_title_in_a_cell"], 1)

    def test_unequal_titles_keep_a_page_level_estimate_unbiased(self):
        # A 9-page title gaining 0.10 and a 1-page title gaining nothing, one pick: the band's
        # mean gain is 0.09. Picking titles evenly would average 0.05.
        sizes = {("eng", 1900, "big"): 9, ("eng", 1900, "tiny"): 1}
        total = 0.0
        runs = 4000
        for i in range(runs):
            take = qc.allocate(sizes, 1, f"seed-{i}")
            self.assertEqual(sum(take.values()), 1)
            key = next(iter(take))
            row_ = {"page_lang": "eng", "decade": 1900, "lccn": key[2]}
            weight = qc.weigh([("h", row_)], sizes, take, 1)[0]["weight"]
            self.assertEqual(weight, 10)
            total += weight * (0.10 if key[2] == "big" else 0.0) / 10
        self.assertAlmostEqual(total / runs, 0.09, delta=0.005)

    def test_a_cell_with_more_titles_than_picks_gets_exact_picks(self):
        sizes = {("eng", 1900, f"sn{i}"): 1 + i % 5 for i in range(50)}
        take = qc.allocate(sizes, 7, "s")
        self.assertEqual(sum(take.values()), 7)
        self.assertTrue(all(take[k] <= sizes[k] for k in take))

    def test_stratum_pages_are_the_title_cells_eligible_pages(self):
        rows = [row("sn1", 1, i, 0.2) for i in range(1, 11)]
        self.build(rows, cell="4")
        sample = lines(self.d / "out/w-moderate/plan.jsonl")
        self.assertEqual({(p["stratum_pages"], p["weight"]) for p in sample}, {(10, 2.5)})

    def test_the_draw_is_not_the_worst_of_the_band(self):
        rows = [row("sn1", 1, i, 0.1 + i / 1000) for i in range(1, 141)]
        self.build(rows, cell="40")
        rates = sorted(p["damage_rate"] for p in lines(self.d / "out/w-moderate/plan.jsonl"))
        self.assertLess(rates[0], 0.15)
        self.assertLess(rates[len(rates) // 2], 0.2)

    def test_the_list_is_in_hash_order_and_uses_the_samplers_hash(self):
        self.build(fixture())
        docs = [p["doc_id"] for p in lines(self.d / "out/w-moderate/plan.jsonl")]
        hashes = [qc.order("s", d) for d in docs]
        self.assertEqual(hashes, sorted(hashes))
        # assess::sample::order: sha256("{seed}\0ocr\0{doc_id}").
        self.assertEqual(qc.order("a", "b"), hashlib.sha256(b"a\0ocr\0b").hexdigest())


class DeterminismTest(Built):
    def contents(self, name):
        return {str(p.relative_to(self.d / name)): p.read_bytes() for p in sorted((self.d / name).rglob("*"))
                if p.is_file() and p.suffix != ".csv"}

    def test_the_same_input_and_seed_give_the_same_bytes_in_any_row_order(self):
        rows = fixture()
        self.build(rows, name="one")
        shuffled = rows[:]
        random.Random(7).shuffle(shuffled)
        self.build(shuffled, name="two")
        a, b = self.contents("one"), self.contents("two")
        a.pop("summary.json"), b.pop("summary.json")  # it names the input file's digest
        self.assertEqual(a, b)

    def test_another_seed_draws_another_sample_and_leaves_the_worst_alone(self):
        self.build(fixture(), name="one", seed="s")
        self.build(fixture(), name="two", seed="t")
        one, two = self.contents("one"), self.contents("two")
        self.assertEqual(one["w/plan.jsonl"], two["w/plan.jsonl"])
        self.assertNotEqual(one["w-moderate/plan.jsonl"], two["w-moderate/plan.jsonl"])


class ExclusionTest(Built):
    def test_campaign_pages_stay_unless_asked_and_are_counted(self):
        rows = [row("sn1", 1, 1, 0.5, in_campaign="t", lang="ger"), row("sn2", 1, 1, 0.2, in_campaign="t")]
        s = self.build(rows)
        self.assertEqual((s["worst"]["pages"], s["moderate"]["pages"]), (1, 1))
        self.assertEqual(s["population"]["worst"]["excluded"], {})
        self.assertEqual(s["population"]["worst"]["in_a_campaign"], 1)
        self.assertFalse(s["campaign_exclusions"])

    def test_our_output_excludes_even_when_the_summary_is_behind(self):
        s = self.build([row("sn1", 1, 1, 0.5, has_ocr="t"), row("sn1", 1, 2, 0.5), row("sn2", 1, 1, 0.2, has_ocr="t")])
        self.assertEqual(s["worst"]["pages"], 1)
        self.assertEqual(s["population"]["worst"]["excluded"], {"quality.ocr_outputs": 1})
        self.assertEqual(s["population"]["moderate"]["excluded"], {"quality.ocr_outputs": 1})

    def test_pages_ocr_holds_are_excluded_in_both_bands(self):
        queued, imported, kept, moderate = (row("sn1", 1, 1, 0.5), row("sn1", 1, 2, 0.5), row("sn1", 1, 3, 0.5),
                                            row("sn2", 1, 1, 0.2))
        with mock.patch.object(qc, "ocr_pages", return_value={queued[0], imported[0], moderate[0]}):
            s = self.build([queued, imported, kept, moderate])
        self.assertEqual([p["doc_id"] for p in lines(self.d / "out/w/plan.jsonl")], [kept[0]])
        self.assertEqual(s["moderate"]["pages"], 0)
        self.assertEqual(s["population"]["worst"]["excluded"], {"OCR queue or import": 2})
        self.assertEqual(s["ocr_state"]["docs"], 3)

    def test_outstanding_and_imported_pages_are_held_and_failed_ones_are_not(self):
        at = dt.datetime(2026, 10, 24, 3, tzinfo=dt.timezone.utc)
        early = at - dt.timedelta(hours=1)
        doc = "sn1_1900-01-01_ed-1_seq-{}".format
        queued = [{"doc_id": doc(i), "reason": "bulk", "queued_at": early} for i in (1, 2, 3)]
        outcomes = [{"doc_id": doc(2), "outcome": "imported", "cause": None, "at": at},
                    {"doc_id": doc(3), "outcome": "dead_lettered", "cause": "permanent", "at": at}]
        (self.state / "queued").mkdir()
        (self.state / "outcomes").mkdir()
        (self.state / "queued/a.parquet").touch()
        (self.state / "outcomes/a.parquet").touch()
        with mock.patch.object(qc, "parquet_rows", lambda path, columns: queued if "queued" in path else outcomes):
            self.assertEqual(qc.ocr_pages(str(self.state)), {doc(1), doc(2)})

    def test_the_flag_turns_on_campaign_rows_and_lists(self):
        worst = row("sn1", 1, 1, 0.5, in_campaign="t")
        listed = row("sn1", 1, 2, 0.5)
        kept = row("sn1", 1, 3, 0.5)
        moderate_listed = row("sn2", 1, 1, 0.2)
        moderate_kept = row("sn2", 1, 2, 0.2)
        done = self.d / "done.txt"
        done.write_text(f"{listed[0]}\tc00001\tkeep/x\n{moderate_listed[0]}\tc00002\n")
        s = self.build([worst, listed, kept, moderate_listed, moderate_kept], "--campaign-exclusions",
                       "--exclude-list", str(done))
        self.assertEqual([p["doc_id"] for p in lines(self.d / "out/w/plan.jsonl")], [kept[0]])
        self.assertEqual([p["doc_id"] for p in lines(self.d / "out/w-moderate/plan.jsonl")], [moderate_kept[0]])
        self.assertEqual(s["population"]["worst"]["pages"], 3)
        self.assertEqual(s["population"]["worst"]["excluded"],
                         {str(done): 1, "quality.unit_pages or quality.staging": 1})
        self.assertEqual(s["population"]["moderate"]["excluded"], {str(done): 1})
        self.assertEqual(lines(self.d / "out/w-moderate/plan.jsonl")[0]["stratum_pages"], 1)
        self.assertEqual(s["exclude_lists"][str(done)],
                         {"docs": 2, "sha256": hashlib.sha256(done.read_bytes()).hexdigest()})

    def test_a_list_without_the_flag_is_refused(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            self.build(fixture(), "--exclude-list", str(self.d / "done.txt"))

    def test_a_missing_exclusion_list_stops_the_build(self):
        with self.assertRaises(SystemExit):
            self.build(fixture(), "--campaign-exclusions", "--exclude-list", str(self.d / "none.txt"))

    def test_a_bad_doc_id_is_counted_not_dropped_silently(self):
        s = self.build([["Bad Id", "ak_a", 1, "sn1", "1900-01-01", 1900, "eng", "detected", 0.5, "worst", "f", "f"]])
        self.assertEqual((s["bad_doc_ids"], s["worst"]["pages"]), (1, 0))


class FormatTest(Built):
    def test_pages_are_what_the_feed_reads_and_the_index_fills_the_scan(self):
        rows = [row("sn1", 1, 1, 0.5, lang="ger", batch="rp_b", version=2), row("sn1", 1, 2, 0.4, batch="rp_b", version=2)]
        index = self.d / "index.jsonl"
        index.write_text(json.dumps({"doc_id": rows[0][0], "batch": "rp_b", "batch_version": 2, "bytes": 9,
                                     "fetched_at": "t", "format": "jp2", "sha256": "ab"}) + "\n")
        s = self.build(rows, "--index", str(index))
        pages = lines(self.d / "out/w/pages.jsonl")
        self.assertEqual(len(pages), 1)
        self.assertEqual(set(pages[0]), SELECT_KEYS)
        self.assertEqual(pages[0]["scan"], "keep/rp_b/sn1/1900-01-01/1/1.jp2")
        self.assertEqual(pages[0]["lang"], "deu")
        got = qc.quality_feed.sample_row(pages[0])
        self.assertEqual((got["batch"], got["batch_version"], got["lang"], got["decade"], got["year"]),
                         ("rp_b", 2, "deu", 1900, 1900))
        plan = lines(self.d / "out/w/plan.jsonl")
        self.assertEqual((plan[0]["page_lang"], plan[1]["scan"], plan[1]["bytes"]), ("ger", None, None))
        self.assertEqual((s["worst"]["on_scan_server"], s["worst"]["to_fetch"]), (1, 1))
        fetched = json.loads((self.d / "out/w/manifests/m00001.parquet").read_bytes())
        self.assertEqual([r["doc_id"] for r in fetched], [rows[1][0]])

    def test_a_scan_of_another_batch_version_is_not_the_pages(self):
        rows = [row("sn1", 1, 1, 0.5, batch="rp_b", version=2)]
        index = self.d / "index.jsonl"
        index.write_text(json.dumps({"doc_id": rows[0][0], "batch": "rp_b", "batch_version": 1, "bytes": 9,
                                     "sha256": "ab"}) + "\n")
        s = self.build(rows, "--index", str(index))
        self.assertEqual((s["worst"]["on_scan_server"], s["worst"]["to_fetch"]), (0, 1))
        self.assertIsNone(lines(self.d / "out/w/plan.jsonl")[0]["sha256"])

    def test_manifest_rows_are_the_quality_pass_rows(self):
        src = self.d / "in.csv"
        write_csv(src, [row("sn1", 3, 4, 0.5, lang="ger", batch="rp_b", version=2, ed=2)])
        r = next(qc.read_rows(str(src)))
        got = qc.manifest_row(r)
        self.assertEqual(got["lang"], "eng")  # the buffer set
        self.assertEqual((got["batch"], got["batch_version"], got["edition"], got["seq"]), ("rp_b", 2, 2, 4))
        self.assertEqual(got["item_url"], "https://www.loc.gov/item/sn1/1900-01-03/ed-2/")
        self.assertEqual(got["decision"], "detected")

    def test_slices_end_where_issues_do(self):
        rows = [{"doc_id": f"sn1_1900-01-0{day}_ed-1_seq-{seq}"} for day in (1, 2, 3) for seq in (1, 2, 3)]
        got = qc.manifest_slices(rows, 2)
        self.assertEqual([len(s) for s in got], [3, 3, 3])
        self.assertEqual(qc.manifest_slices([], 2), [])

    def test_a_manifest_never_crosses_a_tier(self):
        rows = [row("sn1", 1, 1, 0.9, batch="zz_high"), row("sn2", 1, 1, 0.3, batch="aa_low")]
        self.build(rows, "--manifest-pages", "100")
        first = json.loads((self.d / "out/w/manifests/m00001.parquet").read_bytes())
        second = json.loads((self.d / "out/w/manifests/m00002.parquet").read_bytes())
        self.assertEqual(([r["batch"] for r in first], [r["batch"] for r in second]), (["zz_high"], ["aa_low"]))

    def test_summary_counts_by_band_language_and_decade(self):
        s = self.build(fixture())
        self.assertEqual(s["population"]["worst"]["by_language"], {"eng": 30})
        self.assertEqual(s["population"]["worst"]["by_decade"], {"1900": 30})
        self.assertEqual(s["population"]["moderate"]["pages"], 300)
        self.assertEqual(sum(s["moderate"]["by_language"].values()), s["moderate"]["pages"])
        self.assertEqual(sum(s["moderate"]["by_decade"].values()), s["moderate"]["pages"])
        self.assertEqual(sum(s["worst"]["by_tier"].values()), 30)
        self.assertEqual(json.loads((self.d / "out/summary.json").read_text()), s)


class ExportTest(Built):
    def test_the_definition_is_the_reports(self):
        sql = " ".join(qc.EXPORT_SQL.split())
        self.assertIn("WHERE s.damage_rate > 0.1 AND d.head + d.near >= 50 AND s.ocr_runs = 0", sql)
        self.assertIn("CASE WHEN s.damage_rate > 0.25 THEN 'worst' ELSE 'moderate' END", sql)
        self.assertIn("JOIN quality.damage d USING (doc_id)", sql)
        self.assertIn("quality.ocr_outputs", sql)

    def test_the_export_can_only_read(self):
        sql = qc.EXPORT_SQL.upper()
        self.assertTrue(sql.strip().startswith("BEGIN TRANSACTION READ ONLY;"))
        self.assertIsNone(re.search(r"\b(INSERT|UPDATE|DELETE|CREATE|TEMP|DROP|TRUNCATE|CALL)\b", sql))

    def test_export_passes_read_only_and_keeps_the_dsn_out_of_errors(self):
        dsn = self.d / "dsn"
        dsn.write_text("host=db sslkey=/k\n")
        seen = {}

        def psql(cmd, input, stdout, stderr, env, timeout):
            seen.update(cmd=cmd, env=env, sql=input)
            stdout.write(",".join(qc.COLUMNS).encode() + b"\n")
            return mock.Mock(returncode=0)

        target = self.d / "input.csv"
        with mock.patch.object(qc.subprocess, "run", psql):
            qc.export(str(dsn), str(target))
        self.assertEqual(seen["env"]["PGOPTIONS"], "-c default_transaction_read_only=on")
        self.assertEqual(seen["sql"].decode(), qc.EXPORT_SQL)
        self.assertTrue(target.exists())
        with mock.patch.object(qc.subprocess, "run", lambda *a, **k: mock.Mock(returncode=1, stderr=b"boom")):
            with self.assertRaises(SystemExit) as e:
                qc.export(str(dsn), str(target))
        self.assertNotIn("sslkey", str(e.exception))


class ArgsTest(Built):
    def test_positional_arguments_are_refused(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            qc.parser().parse_args(["--campaign", "w", "--seed", "s", "--out-dir", "o", "--input", "i",
                                    "--ocr-state", "d", "extra"])

    def test_an_outbox_is_refused_as_the_output(self):
        write_csv(self.d / "in.csv", [])
        with self.assertRaises(SystemExit) as e:
            run(["--campaign", "w", "--input", str(self.d / "in.csv"), "--ocr-state", str(self.state),
                 "--out-dir", str(self.d / "outbox" / "w")])
        self.assertIn("outbox", str(e.exception))
        self.assertFalse((self.d / "outbox").exists())

    def test_an_outbox_behind_a_link_is_refused(self):
        write_csv(self.d / "in.csv", [])
        (self.d / "outbox").mkdir()
        (self.d / "scratch").symlink_to(self.d / "outbox")
        with self.assertRaises(SystemExit) as e:
            run(self.args(self.d / "in.csv", "scratch"))
        self.assertIn("outbox", str(e.exception))

    def test_a_rebuild_needs_a_new_directory(self):
        self.build(fixture())
        (self.d / "elsewhere").mkdir()
        with self.assertRaises(SystemExit) as e:
            self.build(fixture())
        self.assertIn("new directory", str(e.exception))
        # A link where a campaign directory would go is refused too.
        (self.d / "two").mkdir()
        (self.d / "two" / "w").symlink_to(self.d / "elsewhere")
        with self.assertRaises(SystemExit):
            self.build(fixture(), name="two")

    def test_a_campaign_name_must_leave_room_for_the_suffix(self):
        write_csv(self.d / "in.csv", [])
        with self.assertRaises(SystemExit):
            run(["--campaign", "x" * 39, "--input", str(self.d / "in.csv"), "--ocr-state", str(self.state),
                 "--out-dir", str(self.d / "o")])

    def test_other_columns_are_refused(self):
        (self.d / "in.csv").write_text("doc_id,batch\n")
        with self.assertRaises(SystemExit):
            run(self.args(self.d / "in.csv", "o"))


@unittest.skipUnless(importlib.util.find_spec("pyarrow"), "needs pyarrow")
class ManifestFileTest(Built):
    def test_a_manifest_has_the_fetchers_format_key(self):
        import pyarrow.parquet as pq

        src = self.d / "in.csv"
        write_csv(src, [row("sn1", 1, 1, 0.5)])
        with contextlib.redirect_stdout(io.StringIO()):
            qc.main(["--seed", "s", *self.args(src, "o")])
        t = pq.read_table(self.d / "o/w/manifests/m00001.parquet")
        self.assertEqual(t.schema.metadata[b"usnm.format"], b"usnm-prefetch-manifest/1")
        self.assertEqual(t.num_rows, 1)

    def test_ocr_state_reads_the_ledgers_columns(self):
        import pyarrow as pa
        import pyarrow.parquet as pq

        at = dt.datetime(2026, 10, 24, tzinfo=dt.timezone.utc)
        (self.state / "queued").mkdir()
        (self.state / "outcomes").mkdir()
        doc = "sn1_1900-01-01_ed-1_seq-1"
        pq.write_table(pa.Table.from_pylist([{"doc_id": doc, "reason": "user", "queued_at": at}]),
                       self.state / "queued/a.parquet")
        pq.write_table(pa.Table.from_pylist([{"doc_id": "sn1_1900-01-01_ed-1_seq-2", "outcome": "imported",
                                              "cause": "", "at": at}]), self.state / "outcomes/a.parquet")
        self.assertEqual(qc.ocr_pages(str(self.state)), {doc, "sn1_1900-01-01_ed-1_seq-2"})


if __name__ == "__main__":
    sys.exit(unittest.main())
