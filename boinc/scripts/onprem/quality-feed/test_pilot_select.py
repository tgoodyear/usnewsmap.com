"""Tests for the pilot draw (#1998): no database. The query itself is checked against a
fixture database in scripts/onprem/quality/ci-check.sh."""
import csv
import io
import json
import tempfile
import unittest
from pathlib import Path
import importlib.util
import sys

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent / "ocr-feed")]
spec = importlib.util.spec_from_file_location("pilot_select", HERE / "pilot_select.py")
ps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ps)


def stratum(decade, band, pages, take, tess=False):
    """The query's rows for one stratum: k from 1 to take."""
    return [{"doc_id": f"d{decade}-{band}-{k:03d}", "decade": decade, "band": band, "lm_oov": 5.0 - k / 1000,
             "damage_rate": 0.3, "tess_read": tess and k % 2 == 0, "stratum_pages": pages, "take": take, "k": k}
            for k in range(1, take + 1)]


def write_draw(path, rows):
    out = io.StringIO()
    w = csv.writer(out, lineterminator="\n")
    w.writerow(ps.COLUMNS)
    for r in rows:
        w.writerow([r["doc_id"], r["decade"], r["band"], r["lm_oov"], r["damage_rate"], "t" if r["tess_read"] else "f",
                    r["stratum_pages"], r["take"], r["k"]])
    Path(path).write_text(out.getvalue())


class TrimTest(unittest.TestCase):
    def test_the_largest_strata_lose_a_page_at_a_time(self):
        takes = {(1890, "worst"): 50, (1900, "worst"): 49, (1910, "low"): 30}
        kept, lost = ps.trim(takes, 120)
        self.assertEqual(sum(kept.values()), 120)
        # 129 -> 120: nine pages, 50 and 49 level out at 45 and 45 (ties to the later stratum).
        self.assertEqual(kept, {(1890, "worst"): 45, (1900, "worst"): 45, (1910, "low"): 30})
        self.assertEqual(sum(lost.values()), 9)
        self.assertEqual(lost, {(1890, "worst"): 5, (1900, "worst"): 4})

    def test_a_draw_at_or_under_n_is_left_alone(self):
        takes = {(1890, "worst"): 10, (1900, "low"): 5}
        self.assertEqual(ps.trim(takes, 15), (takes, {}))
        self.assertEqual(ps.trim(takes, 100)[1], {})

    def test_trimming_is_repeatable(self):
        takes = {(d, b): 31 + d % 7 for d in range(1850, 1930, 10) for b in ("worst", "moderate", "low")}
        self.assertEqual(ps.trim(takes, 500), ps.trim(dict(reversed(list(takes.items()))), 500))


class SelectTest(unittest.TestCase):
    rows = stratum(1890, "worst", 400, 40) + stratum(1900, "low", 100, 30, tess=True) + stratum(1910, "moderate", 10, 10)

    def test_keeps_k_up_to_the_trimmed_take_and_orders_by_progress(self):
        picked, takes, kept, lost, pages = ps.select(self.rows, 70)
        self.assertEqual(len(picked), 70)
        self.assertEqual(sum(lost.values()), 10)
        self.assertEqual(kept, {(1890, "worst"): 30, (1900, "low"): 30, (1910, "moderate"): 10})
        self.assertEqual(pages[(1900, "low")], 100)
        self.assertTrue(all(r["k"] <= r["kept"] for r in picked))
        # Any prefix keeps the strata in proportion: the first half has about half of each.
        half = picked[:35]
        for key, want in ((((1890, "worst")), 15), ((1900, "low"), 15), ((1910, "moderate"), 5)):
            got = sum((r["decade"], r["band"]) == key for r in half)
            self.assertLessEqual(abs(got - want), 1, (key, got))

    def test_the_wf1415_flag_marks_pages_without_choosing_them(self):
        picked, *_ = ps.select(self.rows, 80, {"d1890-worst-001", "not-in-the-draw"})
        self.assertEqual({r["doc_id"] for r in picked if r["wf1415"]}, {"d1890-worst-001"})
        self.assertEqual(len(picked), 80)


class MainTest(unittest.TestCase):
    def test_writes_the_files_and_replays_a_draw_the_same(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            rows = stratum(1890, "worst", 400, 300) + stratum(1900, "low", 400, 250, tess=True) + stratum(1910, "moderate", 12, 12)
            write_draw(tmp / "draw.csv", rows)
            plan = tmp / "plan.jsonl"
            plan.write_text(json.dumps({"doc_id": "d1900-low-002"}) + "\n\n")
            disk = tmp / "disk.txt"
            disk.write_text("d1890-worst-001\nd1890-worst-002\n")
            out = tmp / "out"
            argv = ["--out-dir", str(out), "--input", str(tmp / "draw.csv"), "--n", "500", "--chunk", "200",
                    "--wf1415-plan", str(plan), "--on-disk", str(disk)]
            ps.main(argv)
            notes = json.loads((out / "campaign.json").read_text())
            self.assertEqual((notes["drawn"], notes["pages"], notes["trimmed"]), (562, 500, 62))
            # A draw saved without its metadata does not say who made it.
            self.assertEqual((notes["seed"], notes["floor"], notes["query_sha256"], notes["replayed"]),
                             (None, None, None, True))
            self.assertEqual((notes["on_disk"], notes["to_fetch"], notes["wf1415"]), (2, 498, 1))
            self.assertEqual(notes["members_files"], 3)
            files = sorted(p.name for p in out.glob("members-*.json"))
            self.assertEqual(files, ["members-001.json", "members-002.json", "members-003.json"])
            docs = [d for f in files for d in json.loads((out / f).read_text())]
            self.assertEqual(len(docs), 500)
            self.assertEqual(len(set(docs)), 500)
            self.assertEqual(len(json.loads((out / "members-001.json").read_text())), 200)
            strata = json.loads((out / "strata.json").read_text())
            self.assertEqual(set(strata), set(docs))
            self.assertEqual(strata["d1900-low-002"], {"decade": 1900, "band": "low", "tess_read": True, "wf1415": True})
            with open(out / "pilot.csv") as f:
                got = list(csv.DictReader(f))
            self.assertEqual([r["doc_id"] for r in got], docs)
            self.assertEqual(sum(c["kept"] for c in notes["strata"]), 500)
            self.assertTrue(all(c["kept"] <= c["pages"] for c in notes["strata"]))
            first = (out / "pilot.csv").read_bytes()
            # The same draw gives the same files.
            ps.main(argv)
            self.assertEqual((out / "pilot.csv").read_bytes(), first)

    def test_a_replay_copies_the_draw_and_its_metadata_and_keeps_their_provenance(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            out = tmp / "out"
            out.mkdir()
            (out / "draw.csv").write_text("an older draw\n")
            src = tmp / "saved"
            src.mkdir()
            write_draw(src / "draw.csv", stratum(1890, "worst", 400, 40))
            (src / "draw.meta.json").write_text(json.dumps(
                {"seed": "s-7", "n": 40, "floor": 5, "query_sha256": "f" * 64}))
            ps.main(["--out-dir", str(out), "--input", str(src / "draw.csv"), "--n", "40"])
            self.assertEqual((out / "draw.csv").read_bytes(), (src / "draw.csv").read_bytes())
            self.assertEqual(json.loads((out / "draw.meta.json").read_text())["seed"], "s-7")
            notes = json.loads((out / "campaign.json").read_text())
            self.assertEqual((notes["seed"], notes["floor"], notes["query_sha256"], notes["drawn_with_n"]),
                             ("s-7", 5, "f" * 64, 40))
            # Replaying its own output directory changes nothing and does not fail.
            ps.main(["--out-dir", str(out), "--input", str(out / "draw.csv"), "--n", "40"])
            self.assertEqual(json.loads((out / "campaign.json").read_text())["seed"], "s-7")
            # A replay without metadata removes the older draw's, which no longer applies.
            bare = tmp / "bare"
            bare.mkdir()
            write_draw(bare / "draw.csv", stratum(1900, "low", 400, 40))
            ps.main(["--out-dir", str(out), "--input", str(bare / "draw.csv"), "--n", "40"])
            self.assertFalse((out / "draw.meta.json").exists())
            self.assertIsNone(json.loads((out / "campaign.json").read_text())["seed"])
            self.assertIn("d1900-low-001", (out / "pilot.csv").read_text())

    def test_a_rerun_with_fewer_chunks_leaves_no_old_ones_behind(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            write_draw(tmp / "draw.csv", stratum(1890, "worst", 400, 300))
            out = tmp / "out"
            ps.main(["--out-dir", str(out), "--input", str(tmp / "draw.csv"), "--n", "300", "--chunk", "100"])
            self.assertEqual(len(list(out.glob("members-*.json"))), 3)
            ps.main(["--out-dir", str(out), "--input", str(tmp / "draw.csv"), "--n", "150", "--chunk", "100"])
            self.assertEqual(sorted(p.name for p in out.glob("members-*.json")), ["members-001.json", "members-002.json"])
            docs = [d for p in sorted(out.glob("members-*.json")) for d in json.loads(p.read_text())]
            self.assertEqual(len(docs), 150)

    def test_refuses_a_foreign_csv_and_an_empty_draw(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            (tmp / "bad.csv").write_text("doc_id,decade\n")
            with self.assertRaises(SystemExit):
                ps.main(["--out-dir", str(tmp / "o"), "--input", str(tmp / "bad.csv")])
            write_draw(tmp / "empty.csv", [])
            with self.assertRaises(SystemExit):
                ps.main(["--out-dir", str(tmp / "o"), "--input", str(tmp / "empty.csv")])


class QueryTest(unittest.TestCase):
    sql = ps.read_sql(ps.SQL_FILE)[0]

    def test_the_query_has_the_designs_filters(self):
        for want in ("qe_version = 'qe-v1'", "NOT invalid", "language = 'eng'", "language_source = 'page'",
                     "words >= 50", "ntile(10)", "ORDER BY lm_oov DESC", "r.tenth = 1", "reader = 'ours-tesseract'",
                     "< 0.02", "> 0.25", "> 0.10", "md5(:'seed' || doc_id)", "BEGIN TRANSACTION READ ONLY"):
            self.assertIn(want, self.sql)
        for var in (":n", ":floor", ":'seed'"):
            self.assertIn(var, self.sql)

    def test_a_page_tesseract_scored_with_no_rate_is_not_dropped(self):
        # `s - NULL < 0.02` is NULL and would drop the page; the query asks for the NULL first.
        self.assertIn("t.damage_rate IS NULL OR", self.sql)


if __name__ == "__main__":
    unittest.main()
