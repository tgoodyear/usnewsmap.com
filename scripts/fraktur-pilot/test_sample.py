"""The Fraktur pilot's sampler: seeded, stratified, reproducible, with every exclusion counted."""

from contextlib import redirect_stdout
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import random
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
_spec = importlib.util.spec_from_file_location("fraktur_sample", HERE / "sample.py")
sample = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sample)

SPEC = {
    "allocation": "test",
    "audit": {"cut": 1000, "index": "pages-test", "rule": "test"},
    "exclusions": {r: r for r in sample.REASONS},
    "format": sample.SPEC_FORMAT,
    "frame": {
        "decisions": ["detected"],
        "description": "test",
        "formats": ["jp2", "jpeg"],
        "manifest": "pages-test",
        "min_damage": 0.25,
        "resolution": "full",
        "set": "keep",
    },
    "issue": 325,
    "samples": {
        "dan": {"decades": [1880, 1890], "lang": "dan", "rationale": "test", "size": 6},
        "ger": {"decades": [1850, 1860, 1870], "lang": "ger", "rationale": "test", "size": 20},
    },
    "seed": "test-seed",
    "selection": "test",
}


def row(lccn, date, seq, lang="ger", decision="detected", damage=0.4, batch="b_one", version=1):
    return {
        "batch": batch,
        "batch_version": version,
        "damage_rate": damage,
        "date": date,
        "decision": decision,
        "doc_id": f"{lccn}_{date}_ed-1_seq-{seq}",
        "edition": 1,
        "lang": lang,
        "lccn": lccn,
        "seq": seq,
    }


def sidecar(r, fmt="jp2", **override):
    body = hashlib.sha256(r["doc_id"].encode()).hexdigest()
    side = {
        "batch": r["batch"],
        "batch_version": r["batch_version"],
        "bytes": 1000 + r["seq"],
        "doc_id": r["doc_id"],
        "download_ms": 5,
        "fetched_at": "2026-10-11T00:00:00Z",
        "format": fmt,
        "iiif_base": f"https://images.example/{r['doc_id']}",
        "loc_url": f"https://images.example/{r['doc_id']}.jp2",
        "md5": body[:32] if fmt == "jp2" else None,
        "resolution": "full",
        "sha256": body,
        "user_agent": "test",
    }
    if side["md5"] is None:
        del side["md5"]
    side.update(override)
    return side


class Fixture:
    """A synthetic corpus: German pages over four decades and six titles, Danish pages over two,
    and one page for each exclusion."""

    def __init__(self, root):
        self.root = root
        self.rows, self.sides, self.missing = [], [], set()
        titles = ["sn10000001", "sn10000002", "sn10000003"]
        for decade, n_titles, per_title in ((1850, 1, 4), (1860, 3, 6), (1870, 2, 9), (1880, 2, 3)):
            for t in titles[:n_titles] + (["sn20000001"] if decade == 1870 else []):
                for i in range(per_title):
                    r = row(t, f"{decade + i % 10}-0{1 + i % 9}-15", 1 + i)
                    self.rows.append(r)
                    self.sides.append(sidecar(r, fmt="jpeg" if i % 4 == 3 else "jp2"))
        for decade in (1880, 1890, 1900):
            for i in range(5):
                r = row("sn30000001", f"{decade + i}-02-01", 1, lang="dan", batch="b_dan")
                self.rows.append(r)
                self.sides.append(sidecar(r))
        self.rows.append(row("sn40000001", "1910-01-01", 1, lang="eng"))
        self.special = {}

        def add(name, r, side=None):
            self.rows.append(r)
            self.special[name] = r["doc_id"]
            if side is not None:
                self.sides.append(side)

        add("mixed", row("sn10000001", "1861-05-05", 31, decision="mixed"), None)
        add("below", row("sn10000001", "1862-05-05", 32, damage=0.1), None)
        add("no_sidecar", row("sn10000001", "1863-05-05", 33))
        r = row("sn10000001", "1864-05-05", 34)
        add("batch", r, sidecar(r, batch_version=2))
        r = row("sn10000001", "1865-05-05", 35)
        add("nomd5", r, {k: v for k, v in sidecar(r).items() if k != "md5"})
        r = row("sn10000001", "1866-05-05", 36)
        add("scaled", r, sidecar(r, resolution="scaled:50"))
        r = row("sn10000001", "1867-05-05", 37)
        add("gone", r, sidecar(r))
        self.missing.add(r["doc_id"])
        r = row("sn10000001", "1868-05-05", 38)
        twin = self.sides[len(self.rows) // 2]
        self.twins = {r["doc_id"], twin["doc_id"]}
        self.rows.append(r)
        self.sides.append(sidecar(r, sha256=twin["sha256"]))
        add("era", row("sn10000001", "1849-05-05", 39), None)

    def write(self, rows=None, sides=None, seed=None):
        rows = self.rows if rows is None else rows
        sides = self.sides if sides is None else sides
        spec = dict(SPEC, seed=seed or SPEC["seed"])
        paths = {
            "spec": self.root / "spec.json",
            "manifest": self.root / "manifest.jsonl",
            "sidecars": self.root / "index.jsonl",
            "audit": self.root / "audit.csv.gz",
            "listing": self.root / "listing.tsv",
        }
        paths["spec"].write_text(sample.canonical(spec))
        paths["manifest"].write_text("".join(json.dumps(r) + "\n" for r in rows))
        paths["sidecars"].write_text("".join(json.dumps(s) + "\n" for s in sides) + '{"doc_id": "torn')
        audited = [r["doc_id"] for r in rows if sample.audit_member(r["doc_id"], 1000)]
        with gzip.open(paths["audit"], "wt") as f:
            f.write("doc_id,lccn\n" + "".join(f"{d},x\n" for d in audited))
        rows_by_id = {r["doc_id"]: r for r in rows}
        listing = []
        for s in sides:
            r = rows_by_id.get(s["doc_id"])
            if r is None or s["doc_id"] in self.missing:
                continue
            path = sample.scan_path(r, "keep", s.get("format", "jpeg"))
            listing += [f"{path}\t{s['bytes']}", f"{path}.json\t700"]
        paths["listing"].write_text("\n".join(listing) + "\n")
        return paths


class SampleTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.fixture = Fixture(self.root)

    def run_sampler(self, paths, out="out"):
        out = self.root / out
        argv = [
            "--spec", str(paths["spec"]), "--manifest", str(paths["manifest"]),
            "--sidecars", str(paths["sidecars"]), "--audit-pages", str(paths["audit"]),
            "--scan-listing", str(paths["listing"]), "--out", str(out),
        ]
        with redirect_stdout(io.StringIO()):
            self.assertEqual(sample.main(argv), 0)
        records = [json.loads(line) for line in (out / sample.PAGES).read_text().splitlines()]
        summary = json.loads((out / sample.SUMMARY).read_text())
        return out, records, summary

    def test_allocate_splits_equally_and_refills_from_short_members(self):
        self.assertEqual(sample.allocate(10, ["a", "b", "c"], {"a": 9, "b": 9, "c": 9}), {"a": 4, "b": 3, "c": 3})
        self.assertEqual(sample.allocate(10, ["a", "b", "c"], {"a": 1, "b": 9, "c": 9}), {"a": 1, "b": 5, "c": 4})
        self.assertEqual(sample.allocate(10, ["a", "b", "c"], {"a": 1, "b": 2, "c": 3}), {"a": 1, "b": 2, "c": 3})
        self.assertEqual(sample.allocate(2, ["a", "b", "c"], {"a": 5, "b": 0, "c": 5}), {"a": 1, "b": 0, "c": 1})

    def test_draws_by_decade_and_title_and_counts_every_exclusion(self):
        out, records, summary = self.run_sampler(self.fixture.write())
        ger = summary["samples"]["ger"]
        # 20 over three decades: 7, 7, 6; the 1850s has 4 eligible pages, so 1860s and 1870s share the rest.
        self.assertEqual({d: v["allocated"] for d, v in ger["decades"].items()}, {"1850": 4, "1860": 8, "1870": 8})
        self.assertEqual(ger["selected"], 20)
        self.assertEqual(ger["shortfall"], 0)
        # Within the 1870s, three titles of 9 eligible pages share 8 pages: 3, 3, 2.
        strata_1870 = sorted(s["selected"] for s in ger["strata"] if s["decade"] == 1870)
        self.assertEqual(strata_1870, [2, 3, 3])
        self.assertEqual(ger["decades"]["1870"]["titles_selected"], 3)
        excluded = {reason: sum(by_decade.values()) for reason, by_decade in ger["excluded"].items()}
        self.assertEqual(excluded, {
            "outside_era": 1 + 6,  # 1849, and the 1880s pages
            "mixed_language": 1,
            "below_damage": 1,
            "no_sidecar": 1,
            "sidecar_mismatch": 3,  # batch version, a JP2 with no md5, a scaled scan
            "scan_missing": 1,
            "duplicate_image": 1,
        })
        selected_ids = {r["doc_id"] for r in records}
        self.assertFalse(selected_ids & set(self.fixture.special.values()))
        self.assertLessEqual(len(selected_ids & self.fixture.twins), 1)
        dan = summary["samples"]["dan"]
        self.assertEqual((dan["selected"], dan["excluded"]), (6, {"outside_era": {"1900": 5}}))
        self.assertEqual(summary["inputs"]["sidecars"]["unparsed_lines"], 1)
        self.assertEqual(sample.check(out, self.root / "spec.json"), [])

    def test_records_hold_ids_audit_membership_and_hashes_only(self):
        _, records, summary = self.run_sampler(self.fixture.write())
        self.assertEqual(sorted(records[0]), sorted(sample.RECORD_FIELDS))
        sides = {s["doc_id"]: s for s in self.fixture.sides}
        for r in records:
            side = sides[r["doc_id"]]
            self.assertEqual((r["scan_sha256"], r["scan_bytes"], r["image_id"]), (side["sha256"], side["bytes"], side["iiif_base"]))
            self.assertEqual(r["audit_10pct"], r["audit_scored"])
            self.assertTrue(r["scan_path"].startswith(f"keep/{r['batch']}/{r['lccn']}/{r['date']}/1/"))
        self.assertEqual(summary["samples"]["ger"]["audit_rule_disagreements"], 0)
        self.assertIn("jpeg", {r["scan_format"] for r in records})

    def test_same_inputs_in_any_order_give_the_same_bytes_and_another_seed_another_draw(self):
        paths = self.fixture.write()
        first, records, _ = self.run_sampler(paths, "first")
        shuffled = list(self.fixture.rows)
        random.Random(7).shuffle(shuffled)
        again, _, _ = self.run_sampler(self.fixture.write(rows=shuffled), "again")
        self.assertEqual((first / sample.PAGES).read_bytes(), (again / sample.PAGES).read_bytes())
        a, b = (json.loads((d / sample.SUMMARY).read_text()) for d in (first, again))
        self.assertNotEqual(a["inputs"]["manifest"]["sha256"], b["inputs"]["manifest"]["sha256"])
        del a["inputs"], b["inputs"]  # the input files' bytes differ, their pages don't
        self.assertEqual(a, b)
        _, reseeded, _ = self.run_sampler(self.fixture.write(seed="other-seed"), "reseeded")
        self.assertNotEqual({r["doc_id"] for r in records}, {r["doc_id"] for r in reseeded})

    def test_check_catches_edits_to_a_frozen_directory(self):
        out, records, _ = self.run_sampler(self.fixture.write())
        pages = out / sample.PAGES
        original = pages.read_text()
        lines = original.splitlines(keepends=True)
        pages.write_text("".join(lines[1:]))
        self.assertTrue(any("does not match SHA256SUMS" in p for p in sample.check(out, self.root / "spec.json")))

        def rewrite(text):
            pages.write_text(text)
            sums = (out / sample.SUMS).read_text().splitlines()
            fixed = [f"{sample.sha256_file(pages)}  {sample.PAGES}" if s.endswith(sample.PAGES) else s for s in sums]
            (out / sample.SUMS).write_text("\n".join(fixed) + "\n")
            return sample.check(out, self.root / "spec.json")

        swapped = dict(records[0], doc_id=records[0]["lccn"] + "_1999-01-01_ed-1_seq-1")
        problems = rewrite(json.dumps(swapped, sort_keys=True) + "\n" + "".join(lines[1:]))
        self.assertTrue(any("selection_key" in p for p in problems), problems)
        self.assertTrue(any("doc_id is not" in p for p in problems), problems)
        problems = rewrite(lines[0] + "".join(lines[:1] + lines[2:]))
        self.assertTrue(any("appears twice" in p for p in problems), problems)

    def test_check_requires_every_allocated_page(self):
        out, records, summary = self.run_sampler(self.fixture.write())
        dropped = next(r for r in records if r["sample"] == "ger" and r["decade"] == 1870)
        kept = [r for r in records if r is not dropped]
        (out / sample.PAGES).write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in kept))
        ger = summary["samples"]["ger"]
        ger["decades"]["1870"]["selected"] -= 1
        next(st for st in ger["strata"] if (st["decade"], st["lccn"]) == (1870, dropped["lccn"]))["selected"] -= 1
        ger["selected"] -= 1
        ger["shortfall"] += 1
        summary["pages"] -= 1
        (out / sample.SUMMARY).write_text(sample.canonical(summary))
        sums = {line.split("  ")[1]: line for line in (out / sample.SUMS).read_text().splitlines()}
        for name in (sample.PAGES, sample.SUMMARY):
            sums[name] = f"{sample.sha256_file(out / name)}  {name}"
        (out / sample.SUMS).write_text("".join(sums[n] + "\n" for n in sorted(sums)))
        problems = sample.check(out, self.root / "spec.json")
        self.assertIn("summary.json: ger 1870s: allocation or selected count does not add up", problems)
        self.assertIn(f"summary.json: ger 1870s {dropped['lccn']}: selected count does not add up", problems)

    def test_a_scan_shared_across_samples_stays_with_the_page_with_the_lower_key(self):
        def key(r):
            return sample.selection_key(SPEC["seed"], r["doc_id"])

        sides = {s["doc_id"]: s for s in self.fixture.sides}
        dan = [r for r in self.fixture.rows if r["lang"] == "dan" and r["date"] < "1900"]
        ger = [r for r in self.fixture.rows if r["date"].startswith("187") and r["doc_id"] not in self.fixture.twins]
        for lower in ("ger", "dan"):
            d, g = next((d, g) for d in dan for g in ger if (key(g) < key(d)) == (lower == "ger"))
            keep, lose = (g, d) if lower == "ger" else (d, g)
            shared = sides[keep["doc_id"]]["sha256"]
            changed = [dict(s, sha256=shared) if s["doc_id"] == lose["doc_id"] else s for s in self.fixture.sides]
            _, records, summary = self.run_sampler(self.fixture.write(sides=changed), out=lower)
            dups = {
                name: sum(summary["samples"][name]["excluded"].get("duplicate_image", {}).values())
                for name in ("dan", "ger")
            }
            # The fixture's German twin pair already accounts for one German duplicate.
            self.assertEqual(dups, {"dan": 1, "ger": 1} if lower == "ger" else {"dan": 0, "ger": 2}, lower)
            self.assertNotIn(lose["doc_id"], {r["doc_id"] for r in records})

    @unittest.skipUnless(importlib.util.find_spec("pyarrow"), "needs pyarrow (CI installs it)")
    def test_reads_the_prefetch_manifest_parquet(self):
        import datetime

        import pyarrow as pa
        import pyarrow.parquet as pq

        paths = self.fixture.write()
        _, from_jsonl, _ = self.run_sampler(paths, "jsonl")
        rows = [dict(r, date=datetime.date.fromisoformat(r["date"]), item_url="u") for r in self.fixture.rows]
        pq.write_table(pa.Table.from_pylist(rows), self.root / "manifest.parquet")
        _, from_parquet, _ = self.run_sampler(dict(paths, manifest=self.root / "manifest.parquet"), "parquet")
        self.assertEqual(from_jsonl, from_parquet)


class CommittedManifestTests(unittest.TestCase):
    def test_the_committed_manifest_is_consistent_with_its_spec(self):
        frozen = ROOT / "ocr" / "fraktur-pilot"
        self.assertEqual(sample.check(frozen, frozen / "spec.json"), [])


if __name__ == "__main__":
    unittest.main()
