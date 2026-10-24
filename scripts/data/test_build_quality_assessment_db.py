"""Regression tests for scripts/data/build-quality-assessment-db.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sqlite3
import sys
import tempfile
import types
import unittest

try:
    import duckdb
except ModuleNotFoundError:  # pragma: no cover - bare CI runner without duckdb
    duckdb = None


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/data/build-quality-assessment-db.py"


def load_module():
    if duckdb is None and "duckdb" not in sys.modules:
        sys.modules["duckdb"] = types.ModuleType("duckdb")
    spec = importlib.util.spec_from_file_location("build_quality_assessment_db", SCRIPT)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class BuildQualityAssessmentDbTest(unittest.TestCase):
    def test_campaign_unions_use_union_all_by_name_for_pages_and_crops(self):
        mod = load_module()
        executed: list[str] = []

        class FakeConn:
            def execute(self, sql: str):
                executed.append(" ".join(sql.split()))
                return self

            def fetchall(self):
                return []

            def fetchone(self):
                return (0,)

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            for name in ("v1-wave2", "v1-wave8"):
                cdir = quality_root / name
                cdir.mkdir(parents=True)
                (cdir / "pages.parquet").write_bytes(b"x" * 256)
                (cdir / "crops.parquet").write_bytes(b"x" * 256)
            manifest_pq = root / "manifest.parquet"
            manifest_pq.write_bytes(b"x" * 256)
            out_dir = root / "out"

            orig_connect = getattr(mod.duckdb, "connect", None)
            mod.duckdb.connect = lambda: FakeConn()
            try:
                mod.build_database(quality_root, manifest_pq, out_dir, None)
            finally:
                if orig_connect is not None:
                    mod.duckdb.connect = orig_connect

        pages_create = next(s for s in executed if s.startswith("CREATE TABLE raw_boinc_pages AS"))
        crops_create = next(s for s in executed if s.startswith("CREATE TABLE raw_boinc_crops AS"))
        self.assertIn("UNION ALL BY NAME", pages_create)
        self.assertIn("UNION ALL BY NAME", crops_create)
        for col in (
            "bg_tile_std",
            "otsu_local_diff",
            "img_columns",
            "gutter_clarity",
            "est_line_height_px",
            "vertical_stroke_ratio",
            "numeric_share",
            "short_line_share",
            "fraktur_score",
            "ink_word_coverage",
            "hat_version",
            "est_cer_v1",
            "matched",
        ):
            self.assertTrue(
                any(f"ADD COLUMN IF NOT EXISTS {col}" in s for s in executed),
                f"missing ADD COLUMN IF NOT EXISTS {col}",
            )
        self.assertTrue(
            any(s.startswith("UPDATE raw_boinc_pages SET hat_version = 1") for s in executed),
            "tables without hat_version must read as version 1",
        )

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_a_page_in_several_campaigns_keeps_its_hat_v2_row(self):
        """The v1 row wins every later sort key (LoC words, a determined hat, campaign
        priority); the v2 row still wins (#962)."""
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            con = duckdb.connect()
            doc = "sn83030214-1895-01-01-ed-1-seq-1"

            def page(campaign, version_cols, loc_words, determined, est_cer):
                cdir = quality_root / campaign
                cdir.mkdir(parents=True)
                con.execute(
                    f"""
                    COPY (
                        SELECT
                            '{doc}'::VARCHAR AS doc_id, 'english'::VARCHAR AS "group",
                            'eng'::VARCHAR AS language, 'loc'::VARCHAR AS reader,
                            'loc-alto'::VARCHAR AS source, {loc_words}::BIGINT AS words,
                            6800::BIGINT AS chars, {determined} AS hat_determined,
                            0.0::DOUBLE AS hat_residual, 0.8::DOUBLE AS legibility_ceiling,
                            950::BIGINT AS consensus_words, {est_cer}::DOUBLE AS est_cer,
                            0.9::DOUBLE AS completeness, 0.9::DOUBLE AS precision,
                            0.9::DOUBLE AS findable_content, 0.9::DOUBLE AS findable_places,
                            '{{}}'::VARCHAR AS pairs_json, 3200::BIGINT AS width,
                            4800::BIGINT AS height, 0.2::DOUBLE AS skew_deg, 5400.0::DOUBLE AS blur,
                            185.0::DOUBLE AS contrast, 2.1::DOUBLE AS noise,
                            0.07::DOUBLE AS bleed_through, 0.19::DOUBLE AS ink_density,
                            12::BIGINT AS blocks, 210::BIGINT AS lines, 1180::BIGINT AS line_words,
                            34.0::DOUBLE AS line_height_px, 5::BIGINT AS columns,
                            0.78::DOUBLE AS mean_word_conf, 'cpu'::VARCHAR AS plan_class
                            {version_cols}
                    ) TO '{cdir / "pages.parquet"}' (FORMAT PARQUET)
                    """
                )

            # v1-wave8 (priority 100), legacy: LoC has words and the hat is determined.
            page("v1-wave8", "", 1200, "TRUE", 0.30)
            # A later campaign at default priority, version 2, LoC with one word and an
            # underdetermined hat.
            page("v2-later", ", 2::BIGINT AS hat_version, 0.30::DOUBLE AS est_cer_v1, 0.7::DOUBLE AS matched",
                 1, "FALSE", 0.05)
            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (
                        VALUES ('{doc}', 'sn83030214', '1895-01-01', 1, 1, 'batch_ny_01', 'v1', 'eng', 'reocr', 0.22, 'https://example.org/1')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )
            out_dir = root / "out"
            mod.build_database(quality_root, manifest_pq, out_dir, None)
            got = con.execute(
                f"SELECT campaign, hat_version, est_cer, est_cer_v1 FROM read_parquet('{out_dir / 'boinc_reader_rows.parquet'}')"
            ).fetchall()
            self.assertEqual(got, [("v2-later", 2, 0.05, 0.30)])

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_estimates_and_means_use_one_hat_version(self):
        """A v1-only page next to a v2 page: its hat columns are cleared, its unit's
        estimate stays in est_cer_v1, and the means are v2's alone (#962)."""
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            con = duckdb.connect()
            docs = ["sn83030214-1895-01-01-ed-1-seq-1", "sn83030214-1895-01-02-ed-1-seq-1"]
            for campaign, doc, version_cols, est in (
                ("v1-wave8", docs[0], "", 0.40),
                ("v2-later", docs[1], ", 2::BIGINT AS hat_version, 0.30::DOUBLE AS est_cer_v1", 0.05),
            ):
                cdir = quality_root / campaign
                cdir.mkdir(parents=True)
                con.execute(
                    f"""
                    COPY (
                        SELECT
                            '{doc}'::VARCHAR AS doc_id, 'english'::VARCHAR AS "group",
                            'eng'::VARCHAR AS language, 'loc'::VARCHAR AS reader,
                            'loc-alto'::VARCHAR AS source, 1200::BIGINT AS words,
                            6800::BIGINT AS chars, TRUE AS hat_determined,
                            0.0::DOUBLE AS hat_residual, 0.8::DOUBLE AS legibility_ceiling,
                            950::BIGINT AS consensus_words, {est}::DOUBLE AS est_cer,
                            0.9::DOUBLE AS completeness, 0.9::DOUBLE AS precision,
                            0.9::DOUBLE AS findable_content, 0.9::DOUBLE AS findable_places,
                            '{{}}'::VARCHAR AS pairs_json, 3200::BIGINT AS width,
                            4800::BIGINT AS height, 0.2::DOUBLE AS skew_deg, 5400.0::DOUBLE AS blur,
                            185.0::DOUBLE AS contrast, 2.1::DOUBLE AS noise,
                            0.07::DOUBLE AS bleed_through, 0.19::DOUBLE AS ink_density,
                            12::BIGINT AS blocks, 210::BIGINT AS lines, 1180::BIGINT AS line_words,
                            34.0::DOUBLE AS line_height_px, 5::BIGINT AS columns,
                            0.78::DOUBLE AS mean_word_conf, 'cpu'::VARCHAR AS plan_class
                            {version_cols}
                    ) TO '{cdir / "pages.parquet"}' (FORMAT PARQUET)
                    """
                )
            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (
                        VALUES ('{docs[0]}', 'sn83030214', '1895-01-01', 1, 1, 'batch_ny_01', 'v1', 'eng', 'reocr', 0.22, 'https://example.org/1'),
                               ('{docs[1]}', 'sn83030214', '1895-01-02', 1, 1, 'batch_ny_01', 'v1', 'eng', 'reocr', 0.22, 'https://example.org/2')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )
            out_dir = root / "out"
            stats = mod.build_database(quality_root, manifest_pq, out_dir, None)
            self.assertEqual(stats["hat_version"], 2)
            rows = con.execute(
                f"SELECT doc_id, hat_version, est_cer, est_cer_v1, hat_determined FROM read_parquet('{out_dir / 'boinc_reader_rows.parquet'}') ORDER BY doc_id"
            ).fetchall()
            self.assertEqual(rows, [(docs[0], 1, None, 0.40, None), (docs[1], 2, 0.05, 0.30, True)])
            pages = con.execute(
                f"SELECT doc_id, hat_version, loc_est_cer FROM read_parquet('{out_dir / 'page_quality_joined.parquet'}') ORDER BY doc_id"
            ).fetchall()
            self.assertEqual(pages, [(docs[0], 1, None), (docs[1], 2, 0.05)])
            mean = con.execute(
                f"SELECT mean_loc_est_cer FROM read_parquet('{out_dir / 'title_quality_summary.parquet'}') WHERE lccn = 'sn83030214'"
            ).fetchall()
            self.assertEqual(mean, [(0.05,)])

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_mixed_old_and_new_campaign_schemas_preserve_nulls_for_legacy_measurements(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            legacy_dir = quality_root / "v1-wave2"
            reconciled_dir = quality_root / "v1-wave8"
            legacy_dir.mkdir(parents=True)
            reconciled_dir.mkdir(parents=True)

            con = duckdb.connect()

            # Legacy campaign pages.parquet: lacks the new image & lexical columns.
            con.execute(
                f"""
                COPY (
                    SELECT
                        'sn83030214-1895-01-01-ed-1-seq-1'::VARCHAR AS doc_id,
                        'english'::VARCHAR AS "group",
                        'eng'::VARCHAR AS language,
                        'loc'::VARCHAR AS reader,
                        'loc-alto'::VARCHAR AS source,
                        1200::BIGINT AS words,
                        6800::BIGINT AS chars,
                        TRUE AS hat_determined,
                        0.08::DOUBLE AS hat_residual,
                        0.82::DOUBLE AS legibility_ceiling,
                        950::BIGINT AS consensus_words,
                        0.14::DOUBLE AS est_cer,
                        0.88::DOUBLE AS completeness,
                        0.91::DOUBLE AS precision,
                        0.87::DOUBLE AS findable_content,
                        0.85::DOUBLE AS findable_places,
                        '{{"ours-tesseract":{{"cer_a":0.11,"wer_a":0.18,"dice":0.90}}}}'::VARCHAR AS pairs_json,
                        3200::BIGINT AS width,
                        4800::BIGINT AS height,
                        0.2::DOUBLE AS skew_deg,
                        5400.0::DOUBLE AS blur,
                        185.0::DOUBLE AS contrast,
                        2.1::DOUBLE AS noise,
                        0.07::DOUBLE AS bleed_through,
                        0.19::DOUBLE AS ink_density,
                        12::BIGINT AS blocks,
                        210::BIGINT AS lines,
                        1180::BIGINT AS line_words,
                        34.0::DOUBLE AS line_height_px,
                        5::BIGINT AS columns,
                        0.78::DOUBLE AS mean_word_conf,
                        'cpu'::VARCHAR AS plan_class
                ) TO '{legacy_dir / "pages.parquet"}' (FORMAT PARQUET)
                """
            )

            # Reconciled campaign pages.parquet: includes all v1.3 image and lexical columns
            # in a different column order.
            con.execute(
                f"""
                COPY (
                    SELECT
                        'sn83045462-1910-05-02-ed-1-seq-3'::VARCHAR AS doc_id,
                        'german'::VARCHAR AS "group",
                        'deu'::VARCHAR AS language,
                        'ours-tesseract'::VARCHAR AS reader,
                        'ocr-out:frak2021'::VARCHAR AS source,
                        1500::BIGINT AS words,
                        8900::BIGINT AS chars,
                        TRUE AS hat_determined,
                        0.05::DOUBLE AS hat_residual,
                        0.89::DOUBLE AS legibility_ceiling,
                        1250::BIGINT AS consensus_words,
                        0.09::DOUBLE AS est_cer,
                        0.93::DOUBLE AS completeness,
                        0.94::DOUBLE AS precision,
                        0.92::DOUBLE AS findable_content,
                        0.90::DOUBLE AS findable_places,
                        0.11::DOUBLE AS cer_vs_loc,
                        0.17::DOUBLE AS wer_vs_loc,
                        0.91::DOUBLE AS dice_vs_loc,
                        1.04::DOUBLE AS word_ratio_vs_loc,
                        0.12::DOUBLE AS damage_rate,
                        0.27::DOUBLE AS function_share,
                        0.01::DOUBLE AS garbage_share,
                        0.04::DOUBLE AS numeric_share,
                        0.31::DOUBLE AS short_line_share,
                        0.19::DOUBLE AS fraktur_score,
                        0.96::DOUBLE AS ink_word_coverage,
                        980::BIGINT AS known_words,
                        '{{}}'::VARCHAR AS pairs_json,
                        3800::BIGINT AS width,
                        5400::BIGINT AS height,
                        0.4::DOUBLE AS skew_deg,
                        6100.0::DOUBLE AS blur,
                        178.0::DOUBLE AS contrast,
                        2.8::DOUBLE AS noise,
                        0.09::DOUBLE AS bleed_through,
                        0.21::DOUBLE AS ink_density,
                        11.5::DOUBLE AS bg_tile_std,
                        0.03::DOUBLE AS otsu_local_diff,
                        6::BIGINT AS img_columns,
                        0.88::DOUBLE AS gutter_clarity,
                        29.5::DOUBLE AS est_line_height_px,
                        0.24::DOUBLE AS vertical_stroke_ratio,
                        18::BIGINT AS blocks,
                        260::BIGINT AS lines,
                        1480::BIGINT AS line_words,
                        30.0::DOUBLE AS line_height_px,
                        6::BIGINT AS columns,
                        0.76::DOUBLE AS mean_word_conf,
                        'native_cpu'::VARCHAR AS plan_class
                ) TO '{reconciled_dir / "pages.parquet"}' (FORMAT PARQUET)
                """
            )

            # Mixed-schema crops.parquet across legacy and reconciled campaigns.
            con.execute(
                f"""
                COPY (
                    SELECT
                        'sn83030214-1895-01-01-ed-1-seq-1'::VARCHAR AS doc_id,
                        'crop-0'::VARCHAR AS id,
                        'body'::VARCHAR AS stratum,
                        'loc'::VARCHAR AS reader,
                        0.15::DOUBLE AS cer_vs_vlm,
                        0.86::DOUBLE AS dice_vs_vlm,
                        0.13::DOUBLE AS hat_est_cer
                ) TO '{legacy_dir / "crops.parquet"}' (FORMAT PARQUET)
                """
            )
            con.execute(
                f"""
                COPY (
                    SELECT
                        'crop-1'::VARCHAR AS id,
                        'sn83045462-1910-05-02-ed-1-seq-3'::VARCHAR AS doc_id,
                        'ours-tesseract'::VARCHAR AS reader,
                        'headline'::VARCHAR AS stratum,
                        0.07::DOUBLE AS cer_vs_vlm,
                        0.94::DOUBLE AS dice_vs_vlm,
                        0.06::DOUBLE AS hat_est_cer,
                        0.98::DOUBLE AS ink_word_coverage
                ) TO '{reconciled_dir / "crops.parquet"}' (FORMAT PARQUET)
                """
            )

            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (
                        VALUES
                            ('sn83030214-1895-01-01-ed-1-seq-1', 'sn83030214', '1895-01-01', 1, 1, 'batch_ny_01', 'v1', 'eng', 'reocr', 0.22, 'https://example.org/1'),
                            ('sn83045462-1910-05-02-ed-1-seq-3', 'sn83045462', '1910-05-02', 1, 3, 'batch_de_02', 'v1', 'ger', 'reocr', 0.18, 'https://example.org/2')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )

            out_dir = root / "out"
            sqlite_out = root / "assessment.sqlite"
            stats = mod.build_database(quality_root, manifest_pq, out_dir, sqlite_out)

            self.assertEqual(stats["campaigns"], 2)
            self.assertEqual(stats["unique_boinc_pages"], 2)
            self.assertEqual(stats["boinc_reader_rows"], 2)

            rows_pq = out_dir / "boinc_reader_rows.parquet"
            legacy_row = con.execute(
                f"""
                SELECT
                    bg_tile_std,
                    otsu_local_diff,
                    img_columns,
                    gutter_clarity,
                    est_line_height_px,
                    vertical_stroke_ratio,
                    numeric_share,
                    short_line_share,
                    fraktur_score,
                    ink_word_coverage
                FROM read_parquet('{rows_pq}')
                WHERE doc_id = 'sn83030214-1895-01-01-ed-1-seq-1'
                """
            ).fetchone()
            self.assertEqual(legacy_row, (None,) * 10)

            new_row = con.execute(
                f"""
                SELECT
                    bg_tile_std,
                    otsu_local_diff,
                    img_columns,
                    gutter_clarity,
                    est_line_height_px,
                    vertical_stroke_ratio,
                    numeric_share,
                    short_line_share,
                    fraktur_score,
                    ink_word_coverage
                FROM read_parquet('{rows_pq}')
                WHERE doc_id = 'sn83045462-1910-05-02-ed-1-seq-3'
                """
            ).fetchone()
            self.assertEqual(
                new_row,
                (11.5, 0.03, 6, 0.88, 29.5, 0.24, 0.04, 0.31, 0.19, 0.96),
            )

            with sqlite3.connect(sqlite_out) as scon:
                regimes = dict(
                    scon.execute(
                        "SELECT doc_id, router_regime FROM page_quality_joined"
                    ).fetchall()
                )
            self.assertEqual(regimes["sn83030214-1895-01-01-ed-1-seq-1"], "clean_standard")
            self.assertEqual(
                regimes["sn83045462-1910-05-02-ed-1-seq-3"], "multi_script_fraktur"
            )

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_reader_language_summary_rolls_up_pages_and_crops_per_language_and_reader(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            cdir = quality_root / "v1-wave9"
            cdir.mkdir(parents=True)
            con = duckdb.connect()

            # Two English pages (loc, ours-surya) and one German page (loc, ours-tesseract, ours-surya).
            con.execute(
                f"""
                COPY (
                    SELECT *, 'cpu'::VARCHAR AS plan_class, '{{}}'::VARCHAR AS pairs_json,
                        0.8::DOUBLE AS findable_places, 0.1::DOUBLE AS damage_rate,
                        0.2::DOUBLE AS function_share, 0.01::DOUBLE AS garbage_share,
                        500::BIGINT AS known_words, 900::BIGINT AS consensus_words,
                        0.8::DOUBLE AS legibility_ceiling, 3000::BIGINT AS width,
                        4000::BIGINT AS height, 0.1::DOUBLE AS skew_deg,
                        5000.0::DOUBLE AS blur, 180.0::DOUBLE AS contrast,
                        2.0::DOUBLE AS noise, 0.05::DOUBLE AS bleed_through,
                        0.2::DOUBLE AS ink_density, 10::BIGINT AS blocks,
                        200::BIGINT AS lines, 900::BIGINT AS line_words,
                        30.0::DOUBLE AS line_height_px, 5::BIGINT AS columns,
                        0.8::DOUBLE AS mean_word_conf FROM (VALUES
                        ('doc-en-1', 'english', 'eng', 'loc', 1000, TRUE, 0.10, 0.90, 0.92, 0.80, NULL, NULL),
                        ('doc-en-1', 'english', 'eng', 'ours-surya', 1100, TRUE, 0.04, 0.96, 0.97, 0.90, 0.06, 0.94),
                        ('doc-en-2', 'english', 'eng', 'loc', 900, FALSE, 0.20, 0.80, 0.82, 0.70, NULL, NULL),
                        ('doc-en-2', 'english', 'eng', 'ours-surya', 950, FALSE, 0.08, 0.92, 0.93, 0.80, 0.10, 0.90),
                        ('doc-de-1', 'german', 'deu', 'loc', 800, TRUE, 0.30, 0.70, 0.72, 0.60, NULL, NULL),
                        ('doc-de-1', 'german', 'deu', 'ours-tesseract', 820, TRUE, 0.12, 0.88, 0.90, 0.75, 0.15, 0.85),
                        ('doc-de-1', 'german', 'deu', 'ours-surya', 830, TRUE, 0.06, 0.94, 0.95, 0.85, 0.20, 0.80)
                    ) AS t(doc_id, "group", language, reader, words, hat_determined, est_cer,
                           completeness, precision, findable_content, cer_vs_loc, dice_vs_loc)
                ) TO '{cdir / "pages.parquet"}' (FORMAT PARQUET)
                """
            )
            # Crops carry no language: it comes from the page rows of the same doc_id.
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (VALUES
                        ('doc-en-1', 'c0', 'body', 'vlm', 'qwen text', 'qwen text', NULL, NULL, NULL),
                        ('doc-en-1', 'c0', 'body', 'surya', 'surya text', 'qwen text', 0.05, 0.95, 0.04),
                        ('doc-en-2', 'c1', 'body', 'surya', 'surya text', 'qwen text', 0.15, 0.85, 0.08),
                        ('doc-en-2', 'c1', 'body', 'loc', 'loc text', NULL, 0.30, 0.70, 0.25),
                        ('doc-de-1', 'c2', 'headline', 'surya', 'surya text', 'qwen text', 0.20, 0.80, 0.10)
                    ) AS t(doc_id, crop_id, stratum, reader, text, vlm_text, cer_vs_vlm, dice_vs_vlm, hat_est_cer)
                ) TO '{cdir / "crops.parquet"}' (FORMAT PARQUET)
                """
            )
            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (VALUES
                        ('doc-en-1', 'sn1', '1895-01-01', 1, 1, 'b1', 'v1', 'eng', 'reocr', 0.2, 'u1'),
                        ('doc-en-2', 'sn1', '1895-01-02', 1, 1, 'b1', 'v1', 'eng', 'reocr', 0.2, 'u2'),
                        ('doc-de-1', 'sn2', '1910-05-02', 1, 3, 'b2', 'v1', 'ger', 'reocr', 0.2, 'u3')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )

            out_dir = root / "out"
            sqlite_path = root / "assessment.sqlite"
            mod.build_database(quality_root, manifest_pq, out_dir, sqlite_path)

            rows = con.execute(
                f"""
                SELECT language, reader, pages, mean_est_cer, median_est_cer, mean_cer_vs_loc,
                       pages_hat_determined, crops, mean_crop_hat_est_cer, mean_cer_vs_vlm,
                       crops_with_vlm
                FROM read_parquet('{out_dir / "reader_language_summary.parquet"}')
                ORDER BY language, reader
                """
            ).fetchall()
            by_key = {(r[0], r[1]): r for r in rows}
            self.assertEqual(
                [(r[0], r[1]) for r in rows],
                [
                    ("deu", "loc"),
                    ("deu", "ours-surya"),
                    ("deu", "ours-tesseract"),
                    ("deu", "surya"),
                    ("eng", "loc"),
                    ("eng", "ours-surya"),
                    ("eng", "surya"),
                    ("eng", "vlm"),
                ],
            )
            # English ours-surya: two pages, one hat-determined.
            en_surya = by_key[("eng", "ours-surya")]
            self.assertEqual(en_surya[2], 2)
            self.assertAlmostEqual(en_surya[3], 0.06)
            self.assertAlmostEqual(en_surya[4], 0.06)
            self.assertAlmostEqual(en_surya[5], 0.08)
            self.assertEqual(en_surya[6], 1)
            self.assertEqual(en_surya[7], 0)
            self.assertIsNone(en_surya[8])
            # English surya crops exist only in the crop table: no page measures.
            en_crop = by_key[("eng", "surya")]
            self.assertEqual(en_crop[2], 0)
            self.assertIsNone(en_crop[3])
            self.assertEqual(en_crop[7], 2)
            self.assertAlmostEqual(en_crop[8], 0.06)
            self.assertAlmostEqual(en_crop[9], 0.10)
            self.assertEqual(en_crop[10], 2)
            # The vlm reader has a crop row and no scores; loc crops have no vlm text.
            en_vlm = by_key[("eng", "vlm")]
            self.assertEqual((en_vlm[2], en_vlm[7], en_vlm[10]), (0, 1, 1))
            self.assertIsNone(en_vlm[9])
            self.assertEqual(by_key[("eng", "loc")][7], 1)
            self.assertEqual(by_key[("eng", "loc")][10], 0)
            self.assertAlmostEqual(by_key[("deu", "surya")][9], 0.20)

            group_rows = con.execute(
                f"""
                SELECT COUNT(*) FROM read_parquet('{out_dir / "reader_language_group_summary.parquet"}')
                WHERE language = 'eng' AND "group" = 'english'
                """
            ).fetchone()
            self.assertEqual(group_rows[0], 4)
            summary = (out_dir / "summary.md").read_text(encoding="utf-8")
            self.assertIn("reader_language_summary", summary)

            # Pivoted comparison: one row per language, two columns per reader (sorted).
            cmp_pq = out_dir / "reader_language_comparison.parquet"
            cmp_cols = [
                d[0]
                for d in con.execute(f"DESCRIBE SELECT * FROM read_parquet('{cmp_pq}')").fetchall()
            ]
            self.assertEqual(
                cmp_cols,
                [
                    "language", "pages", "crops",
                    "loc_page_cer", "loc_crop_cer",
                    "ours_surya_page_cer", "ours_surya_crop_cer",
                    "ours_tesseract_page_cer", "ours_tesseract_crop_cer",
                    "surya_page_cer", "surya_crop_cer",
                    "vlm_page_cer", "vlm_crop_cer",
                ],
            )
            cmp_rows = con.execute(
                f"SELECT * FROM read_parquet('{cmp_pq}') ORDER BY language"
            ).fetchall()
            cmp_by_lang = {r[0]: dict(zip(cmp_cols, r)) for r in cmp_rows}
            self.assertEqual(list(cmp_by_lang), ["deu", "eng"])
            eng = cmp_by_lang["eng"]
            self.assertEqual((eng["pages"], eng["crops"]), (2, 2))
            self.assertAlmostEqual(eng["ours_surya_page_cer"], 0.06)
            # ours-surya has pages only; surya has crops only; loc has both.
            self.assertIsNone(eng["ours_surya_crop_cer"])
            self.assertIsNone(eng["surya_page_cer"])
            self.assertAlmostEqual(eng["surya_crop_cer"], 0.06)
            self.assertAlmostEqual(eng["loc_page_cer"], 0.15)
            self.assertAlmostEqual(eng["loc_crop_cer"], 0.25)
            self.assertIsNone(eng["ours_tesseract_page_cer"])
            self.assertIsNone(eng["vlm_page_cer"])
            self.assertIsNone(eng["vlm_crop_cer"])
            deu = cmp_by_lang["deu"]
            self.assertEqual((deu["pages"], deu["crops"]), (1, 1))
            self.assertAlmostEqual(deu["ours_tesseract_page_cer"], 0.12)
            self.assertAlmostEqual(deu["surya_crop_cer"], 0.10)

            self.assertIn("## Reader Comparison by Language (`reader_language_comparison`)", summary)
            self.assertIn("### Page-level estimated CER", summary)
            self.assertIn("### Crop-level hat estimated CER", summary)
            self.assertIn("| Language | Pages | loc | ours-surya | ours-tesseract | surya | vlm |", summary)
            self.assertIn("| Language | Crops | loc | ours-surya | ours-tesseract | surya | vlm |", summary)
            self.assertIn("| `eng` | 2 | 0.15 | 0.06 | - | - | - |", summary)
            self.assertIn("| `eng` | 2 | 0.25 | - | - | 0.06 | - |", summary)
            self.assertLess(
                summary.index("Reader Quality by Language"),
                summary.index("Reader Comparison by Language"),
            )
            scon = sqlite3.connect(sqlite_path)
            try:
                sqlite_cols = [r[1] for r in scon.execute("PRAGMA table_info(reader_language_comparison)")]
                self.assertEqual(sqlite_cols, cmp_cols)
                self.assertAlmostEqual(
                    scon.execute(
                        "SELECT ours_surya_page_cer FROM reader_language_comparison WHERE language = 'eng'"
                    ).fetchone()[0],
                    0.06,
                )
            finally:
                scon.close()

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_reader_language_summary_averages_all_null_varchar_crop_scores(self):
        # The collector's to_parquet types an all-null column as UTF-8, so a campaign whose
        # crops were never scored against a VLM arrives with VARCHAR score columns.
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            cdir = quality_root / "v1-wave10"
            cdir.mkdir(parents=True)
            con = duckdb.connect()
            con.execute(
                f"""
                COPY (
                    SELECT *, 'cpu'::VARCHAR AS plan_class, '{{}}'::VARCHAR AS pairs_json,
                        0.8::DOUBLE AS findable_places, 0.1::DOUBLE AS damage_rate,
                        0.2::DOUBLE AS function_share, 0.01::DOUBLE AS garbage_share,
                        500::BIGINT AS known_words, 900::BIGINT AS consensus_words,
                        0.8::DOUBLE AS legibility_ceiling, 3000::BIGINT AS width,
                        4000::BIGINT AS height, 0.1::DOUBLE AS skew_deg,
                        5000.0::DOUBLE AS blur, 180.0::DOUBLE AS contrast,
                        2.0::DOUBLE AS noise, 0.05::DOUBLE AS bleed_through,
                        0.2::DOUBLE AS ink_density, 10::BIGINT AS blocks,
                        200::BIGINT AS lines, 900::BIGINT AS line_words,
                        30.0::DOUBLE AS line_height_px, 5::BIGINT AS columns,
                        0.8::DOUBLE AS mean_word_conf FROM (VALUES
                        ('doc-en-1', 'english', 'eng', 'loc', 1000, TRUE, 0.10, 0.90, 0.92, 0.80, NULL, NULL),
                        ('doc-en-1', 'english', 'eng', 'ours-surya', 1100, TRUE, 0.04, 0.96, 0.97, 0.90, 0.06, 0.94)
                    ) AS t(doc_id, "group", language, reader, words, hat_determined, est_cer,
                           completeness, precision, findable_content, cer_vs_loc, dice_vs_loc)
                ) TO '{cdir / "pages.parquet"}' (FORMAT PARQUET)
                """
            )
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (VALUES
                        ('doc-en-1', 'c0', 'body', 'surya', 'surya text', NULL::VARCHAR,
                         NULL::VARCHAR, NULL::VARCHAR, NULL::VARCHAR),
                        ('doc-en-1', 'c0', 'body', 'loc', 'loc text', NULL::VARCHAR,
                         NULL::VARCHAR, NULL::VARCHAR, NULL::VARCHAR)
                    ) AS t(doc_id, crop_id, stratum, reader, text, vlm_text, cer_vs_vlm, dice_vs_vlm, hat_est_cer)
                ) TO '{cdir / "crops.parquet"}' (FORMAT PARQUET)
                """
            )
            self.assertEqual(
                con.execute(f"SELECT typeof(cer_vs_vlm) FROM read_parquet('{cdir / 'crops.parquet'}') LIMIT 1").fetchone()[0],
                "VARCHAR",
            )
            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (VALUES
                        ('doc-en-1', 'sn1', '1895-01-01', 1, 1, 'b1', 'v1', 'eng', 'reocr', 0.2, 'u1')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )
            out_dir = root / "out"
            mod.build_database(quality_root, manifest_pq, out_dir, None)
            rows = con.execute(
                f"""
                SELECT reader, crops, mean_crop_hat_est_cer, mean_cer_vs_vlm, mean_dice_vs_vlm, crops_with_vlm
                FROM read_parquet('{out_dir / "reader_language_summary.parquet"}')
                WHERE language = 'eng' ORDER BY reader
                """
            ).fetchall()
            self.assertEqual(rows, [("loc", 1, None, None, None, 0), ("ours-surya", 0, None, None, None, 0),
                                    ("surya", 1, None, None, None, 0)])

    @unittest.skipIf(duckdb is None, "duckdb not installed in this Python environment")
    def test_reader_language_comparison_counts_distinct_samples_across_readers(self):
        # Campaigns stage only the readers they ask for, so two English pages read by
        # different readers are two pages of the language, not the larger reader's one.
        # A crop counts once per campaign it appears in, as the per-reader counts do.
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            quality_root = root / "quality"
            con = duckdb.connect()
            for campaign, page_rows, crop_rows in (
                ("v1-wave11",
                 "('doc-en-1', 'english', 'eng', 'loc', 1000, TRUE, 0.10, 0.90, 0.92, 0.80, NULL, NULL), "
                 "('doc-en-2', 'english', 'eng', 'ours-tesseract', 900, TRUE, 0.20, 0.80, 0.82, 0.70, 0.3, 0.7)",
                 "('doc-en-1', 'c0', 'body', 'loc', 'loc text', NULL::VARCHAR, NULL::DOUBLE, NULL::DOUBLE, 0.25), "
                 "('doc-en-2', 'c1', 'body', 'ours-tesseract', 'tess text', NULL::VARCHAR, NULL::DOUBLE, NULL::DOUBLE, 0.35)"),
                ("v1-wave12",
                 "('doc-en-1', 'english', 'eng', 'loc', 1000, TRUE, 0.10, 0.90, 0.92, 0.80, NULL, NULL)",
                 "('doc-en-1', 'c0', 'body', 'loc', 'loc text', NULL::VARCHAR, NULL::DOUBLE, NULL::DOUBLE, 0.25)"),
            ):
                cdir = quality_root / campaign
                cdir.mkdir(parents=True)
                con.execute(
                    f"""
                    COPY (
                        SELECT *, 'cpu'::VARCHAR AS plan_class, '{{}}'::VARCHAR AS pairs_json,
                        0.8::DOUBLE AS findable_places, 0.1::DOUBLE AS damage_rate,
                        0.2::DOUBLE AS function_share, 0.01::DOUBLE AS garbage_share,
                        500::BIGINT AS known_words, 900::BIGINT AS consensus_words,
                        0.8::DOUBLE AS legibility_ceiling, 3000::BIGINT AS width,
                        4000::BIGINT AS height, 0.1::DOUBLE AS skew_deg,
                        5000.0::DOUBLE AS blur, 180.0::DOUBLE AS contrast,
                        2.0::DOUBLE AS noise, 0.05::DOUBLE AS bleed_through,
                        0.2::DOUBLE AS ink_density, 10::BIGINT AS blocks,
                        200::BIGINT AS lines, 900::BIGINT AS line_words,
                        30.0::DOUBLE AS line_height_px, 5::BIGINT AS columns,
                        0.8::DOUBLE AS mean_word_conf FROM (VALUES {page_rows}
                        ) AS t(doc_id, "group", language, reader, words, hat_determined, est_cer,
                               completeness, precision, findable_content, cer_vs_loc, dice_vs_loc)
                    ) TO '{cdir / "pages.parquet"}' (FORMAT PARQUET)
                    """
                )
                con.execute(
                    f"""
                    COPY (
                        SELECT * FROM (VALUES {crop_rows}
                        ) AS t(doc_id, crop_id, stratum, reader, text, vlm_text, cer_vs_vlm, dice_vs_vlm, hat_est_cer)
                    ) TO '{cdir / "crops.parquet"}' (FORMAT PARQUET)
                    """
                )
            manifest_pq = root / "manifest.parquet"
            con.execute(
                f"""
                COPY (
                    SELECT * FROM (VALUES
                        ('doc-en-1', 'sn1', '1895-01-01', 1, 1, 'b1', 'v1', 'eng', 'reocr', 0.2, 'u1'),
                        ('doc-en-2', 'sn1', '1895-01-02', 1, 1, 'b1', 'v1', 'eng', 'reocr', 0.2, 'u2')
                    ) AS t(doc_id, lccn, date, edition, seq, batch, batch_version, lang, decision, damage_rate, item_url)
                ) TO '{manifest_pq}' (FORMAT PARQUET)
                """
            )
            out_dir = root / "out"
            mod.build_database(quality_root, manifest_pq, out_dir, None)
            per_reader = dict(con.execute(
                f"""
                SELECT reader, pages FROM read_parquet('{out_dir / "reader_language_summary.parquet"}')
                WHERE language = 'eng'
                """
            ).fetchall())
            self.assertEqual(per_reader, {"loc": 1, "ours-tesseract": 1})
            row = con.execute(
                f"""
                SELECT pages, crops, loc_page_cer, ours_tesseract_page_cer, loc_crop_cer, ours_tesseract_crop_cer
                FROM read_parquet('{out_dir / "reader_language_comparison.parquet"}')
                WHERE language = 'eng'
                """
            ).fetchone()
            # Two distinct pages; three crop samples (doc-en-1's crop in both campaigns, doc-en-2's in one).
            self.assertEqual(row, (2, 3, 0.1, 0.2, 0.25, 0.35))
            summary = (out_dir / "summary.md").read_text(encoding="utf-8")
            self.assertIn("| `eng` | 2 | 0.1 | 0.2 |", summary)
            self.assertIn("| `eng` | 3 | 0.25 | 0.35 |", summary)


if __name__ == "__main__":
    unittest.main()
