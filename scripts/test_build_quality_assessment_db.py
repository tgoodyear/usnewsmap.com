"""Regression tests for scripts/build-quality-assessment-db.py."""

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


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/build-quality-assessment-db.py"


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
        ):
            self.assertTrue(
                any(f"ADD COLUMN IF NOT EXISTS {col}" in s for s in executed),
                f"missing ADD COLUMN IF NOT EXISTS {col}",
            )

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


if __name__ == "__main__":
    unittest.main()
