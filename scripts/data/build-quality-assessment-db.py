#!/usr/bin/env python3
"""Build the unified Full-Corpus Damage & BOINC Quality Assessment Database.

Joins the full-corpus lexical damage manifest (`manifest.parquet`, 1.92M pages
across 3,664 titles scored by `usnm-ingest ocr-damage`) with all collected
BOINC `usnm_quality` and `usnm_qwen` campaign tables (`audit/quality/*/pages.parquet`
and `crops.parquet`), extracts pairwise `cer_vs_loc` / `wer_vs_loc` / `dice_vs_loc` /
`word_ratio_vs_loc` from `pairs_json` for 2-reader pages, carries `hat_version`
(1 for tables written before the page hat's version 2, #962; a page seen in
several campaigns keeps its version 2 row), and writes:

1. Consolidated Parquet tables (`boinc_reader_rows.parquet`,
   `page_quality_joined.parquet`, `title_quality_summary.parquet`,
   `language_quality_summary.parquet`, `reader_language_summary.parquet`,
   `reader_language_group_summary.parquet`, `reader_language_comparison.parquet`,
   `summary.md`) under `--out-dir`. `reader_language_comparison` pivots
   `reader_language_summary` to one row per language with `<reader>_page_cer` and
   `<reader>_crop_cer` columns (hyphens in reader names become underscores), plus
   `pages` and `crops`, for an apples-to-apples reader comparison across languages.
2. An indexed SQLite database (`--sqlite-out`) containing the full BOINC
   page/reader/crop tables plus title-level and language-level full-corpus
   damage & quality summaries.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import sqlite3
import duckdb


CAMPAIGN_PRIORITY = {
    "v1-wave8": 100,
    "v1-engine-bench": 95,
    "v1-wave7": 90,
    "v1-wave6": 80,
    "v1-wave5": 70,
    "v1-wave4": 60,
    "v1-wave3": 50,
    "v1-wave2": 40,
    "v1-wave1": 30,
    "v1-pilot-mac": 20,
    "v1-pilot-3r": 10,
}


def discover_campaigns(quality_root: Path) -> list[tuple[str, Path, Path | None]]:
    found: list[tuple[str, Path, Path | None]] = []
    for p in sorted(quality_root.iterdir()):
        if not p.is_dir() or p.name == "consolidated":
            continue
        pages_pq = p / "pages.parquet"
        if pages_pq.is_file() and pages_pq.stat().st_size > 200:
            crops_pq = p / "crops.parquet"
            found.append(
                (
                    p.name,
                    pages_pq,
                    crops_pq if (crops_pq.is_file() and crops_pq.stat().st_size > 200) else None,
                )
            )
    return found


def build_database(
    quality_root: Path,
    manifest_parquet: Path,
    out_dir: Path,
    sqlite_path: Path | None,
) -> dict[str, int]:
    out_dir.mkdir(parents=True, exist_ok=True)
    campaigns = discover_campaigns(quality_root)
    if not campaigns:
        raise SystemExit(f"no campaign pages.parquet files found under {quality_root}")

    con = duckdb.connect()
    union_parts = []
    crop_parts = []
    for name, pages_pq, crops_pq in campaigns:
        prio = CAMPAIGN_PRIORITY.get(name, 50)
        escaped_pages = str(pages_pq).replace("'", "''")
        union_parts.append(
            f"SELECT '{name}' AS campaign, {prio} AS campaign_priority, "
            f"* REPLACE (plan_class::VARCHAR AS plan_class) "
            f"FROM read_parquet('{escaped_pages}', union_by_name=true)"
        )
        if crops_pq is not None:
            escaped_crops = str(crops_pq).replace("'", "''")
            crop_parts.append(
                f"SELECT '{name}' AS campaign, * "
                f"FROM read_parquet('{escaped_crops}', union_by_name=true)"
            )

    con.execute(f"CREATE TABLE raw_boinc_pages AS {' UNION ALL BY NAME '.join(union_parts)}")
    for col_name, col_type in [
        ("cer_vs_loc", "DOUBLE"),
        ("wer_vs_loc", "DOUBLE"),
        ("dice_vs_loc", "DOUBLE"),
        ("word_ratio_vs_loc", "DOUBLE"),
        ("damage_rate", "DOUBLE"),
        ("function_share", "DOUBLE"),
        ("garbage_share", "DOUBLE"),
        ("known_words", "BIGINT"),
        ("hat_residual", "DOUBLE"),
        ("legibility_ceiling", "DOUBLE"),
        ("otsu_local_diff", "DOUBLE"),
        ("bg_tile_std", "DOUBLE"),
        ("est_line_height_px", "DOUBLE"),
        ("vertical_stroke_ratio", "DOUBLE"),
        ("fraktur_score", "DOUBLE"),
        ("gutter_clarity", "DOUBLE"),
        ("img_columns", "BIGINT"),
        ("ink_word_coverage", "DOUBLE"),
        ("numeric_share", "DOUBLE"),
        ("short_line_share", "DOUBLE"),
        ("hat_version", "BIGINT"),
        ("est_cer_v1", "DOUBLE"),
        ("matched", "DOUBLE"),
    ]:
        con.execute(f"ALTER TABLE raw_boinc_pages ADD COLUMN IF NOT EXISTS {col_name} {col_type}")
    # Tables written before the page hat's version 2 (#962) carry the unit's
    # whole-page hat: version 1, with est_cer as its own v1 value.
    con.execute(
        "UPDATE raw_boinc_pages SET hat_version = 1, est_cer_v1 = est_cer "
        "WHERE hat_version IS NULL"
    )
    if crop_parts:
        con.execute(f"CREATE TABLE raw_boinc_crops AS {' UNION ALL BY NAME '.join(crop_parts)}")
        for col_name, col_type in [
            ("vlm_text", "VARCHAR"),
            ("cer_vs_vlm", "DOUBLE"),
            ("dice_vs_vlm", "DOUBLE"),
            ("hat_est_cer", "DOUBLE"),
        ]:
            con.execute(
                f"ALTER TABLE raw_boinc_crops ADD COLUMN IF NOT EXISTS {col_name} {col_type}"
            )
    else:
        con.execute(
            "CREATE VIEW raw_boinc_crops AS "
            "SELECT ''::VARCHAR AS campaign, ''::VARCHAR AS doc_id, ''::VARCHAR AS id, "
            "''::VARCHAR AS stratum, ''::VARCHAR AS reader, NULL::VARCHAR AS vlm_text, "
            "NULL::DOUBLE AS cer_vs_vlm, "
            "NULL::DOUBLE AS dice_vs_vlm, NULL::DOUBLE AS hat_est_cer WHERE 1=0"
        )

    # Extract pairwise metrics vs loc / loc-hist from pairs_json and deduplicate
    # per (doc_id, reader), preferring campaigns where loc/loc-hist has real text (>1 word).
    con.execute(
        """
        CREATE TABLE boinc_reader_rows AS
        WITH page_loc_words AS (
            SELECT
                campaign,
                doc_id,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') THEN words ELSE 0 END) AS loc_words
            FROM raw_boinc_pages
            GROUP BY campaign, doc_id
        ),
        ranked AS (
            SELECT
                r.* EXCLUDE (cer_vs_loc, wer_vs_loc, dice_vs_loc, word_ratio_vs_loc),
                plw.loc_words,
                CASE
                    WHEN r.reader IN ('loc', 'loc-hist') THEN CASE WHEN r.words > 1 THEN 0.0 ELSE NULL END
                    WHEN plw.loc_words > 1 THEN COALESCE(
                        r.cer_vs_loc,
                        json_extract(r.pairs_json, '$."loc".cer_a')::DOUBLE,
                        json_extract(r.pairs_json, '$."loc-hist".cer_a')::DOUBLE
                    )
                    ELSE NULL
                END AS cer_vs_loc,
                CASE
                    WHEN r.reader IN ('loc', 'loc-hist') THEN CASE WHEN r.words > 1 THEN 0.0 ELSE NULL END
                    WHEN plw.loc_words > 1 THEN COALESCE(
                        r.wer_vs_loc,
                        json_extract(r.pairs_json, '$."loc".wer_a')::DOUBLE,
                        json_extract(r.pairs_json, '$."loc-hist".wer_a')::DOUBLE
                    )
                    ELSE NULL
                END AS wer_vs_loc,
                CASE
                    WHEN r.reader IN ('loc', 'loc-hist') THEN CASE WHEN r.words > 1 THEN 1.0 ELSE NULL END
                    WHEN plw.loc_words > 1 THEN COALESCE(
                        r.dice_vs_loc,
                        json_extract(r.pairs_json, '$."loc".dice')::DOUBLE,
                        json_extract(r.pairs_json, '$."loc-hist".dice')::DOUBLE
                    )
                    ELSE NULL
                END AS dice_vs_loc,
                CASE
                    WHEN r.reader IN ('loc', 'loc-hist') THEN CASE WHEN r.words > 0 THEN 1.0 ELSE NULL END
                    WHEN plw.loc_words > 1 THEN COALESCE(r.word_ratio_vs_loc, ROUND(r.words::DOUBLE / plw.loc_words::DOUBLE, 6))
                    ELSE NULL
                END AS word_ratio_vs_loc,
                ROW_NUMBER() OVER (
                    PARTITION BY r.doc_id, r.reader
                    ORDER BY r.hat_version DESC, (plw.loc_words > 1) DESC, r.hat_determined DESC NULLS LAST, r.campaign_priority DESC
                ) AS rn
            FROM raw_boinc_pages r
            JOIN page_loc_words plw USING (campaign, doc_id)
        )
        SELECT * EXCLUDE (rn)
        FROM ranked
        WHERE rn = 1
        """
    )

    escaped_manifest = str(manifest_parquet).replace("'", "''")
    con.execute(
        f"""
        CREATE VIEW manifest_pages AS
        SELECT
            doc_id,
            lccn,
            date::VARCHAR AS issue_date,
            edition,
            seq,
            batch,
            batch_version,
            CASE lang
                WHEN 'ger' THEN 'deu'
                WHEN 'cze' THEN 'ces'
                WHEN 'fre' THEN 'fra'
                WHEN 'slo' THEN 'slk'
                WHEN 'rum' THEN 'ron'
                WHEN 'ice' THEN 'isl'
                WHEN 'dut' THEN 'nld'
                WHEN 'gre' THEN 'ell'
                WHEN 'chi' THEN 'zho'
                WHEN 'wel' THEN 'cym'
                ELSE lang
            END AS manifest_lang,
            lang AS manifest_lang_raw,
            decision AS manifest_decision,
            ROUND(damage_rate, 6) AS manifest_damage_rate,
            item_url
        FROM read_parquet('{escaped_manifest}')
        """
    )

    # Page-level consolidated BOINC + corpus damage view
    con.execute(
        """
        CREATE TABLE page_quality_joined AS
        WITH page_agg AS (
            SELECT
                doc_id,
                FIRST(campaign ORDER BY campaign_priority DESC) AS campaign,
                FIRST("group" ORDER BY campaign_priority DESC) AS sample_group,
                FIRST(language ORDER BY campaign_priority DESC) AS boinc_lang,
                COUNT(DISTINCT reader) AS n_readers,
                BOOL_OR(COALESCE(hat_determined, FALSE)) AS hat_determined,
                MAX(legibility_ceiling) AS legibility_ceiling,
                MAX(consensus_words) AS consensus_words,
                MAX(width) AS width,
                MAX(height) AS height,
                MAX(skew_deg) AS skew_deg,
                MAX(blur) AS blur,
                MAX(contrast) AS contrast,
                MAX(noise) AS noise,
                MAX(bleed_through) AS bleed_through,
                MAX(ink_density) AS ink_density,
                MAX(blocks) AS blocks,
                MAX(lines) AS lines,
                MAX(line_words) AS line_words,
                MAX(line_height_px) AS line_height_px,
                MAX(columns) AS columns,
                MAX(mean_word_conf) AS mean_word_conf,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN words END) AS loc_words,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN est_cer END) AS loc_est_cer,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN completeness END) AS loc_completeness,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN findable_content END) AS loc_findable_content,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN findable_places END) AS loc_findable_places,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN damage_rate END) AS loc_damage_rate,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN function_share END) AS loc_function_share,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN garbage_share END) AS loc_garbage_share,
                MAX(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN known_words END) AS loc_known_words,
                MAX(CASE WHEN reader = 'ours-tesseract' THEN words END) AS ours_words,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN est_cer END) AS ours_est_cer,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN completeness END) AS ours_completeness,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN findable_content END) AS ours_findable_content,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN findable_places END) AS ours_findable_places,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN cer_vs_loc END) AS ours_cer_vs_loc,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN dice_vs_loc END) AS ours_dice_vs_loc,
                MAX(CASE WHEN reader = 'ours-tesseract' AND COALESCE(loc_valid, TRUE) THEN word_ratio_vs_loc END) AS ours_word_ratio_vs_loc,
                MAX(CASE WHEN reader = 'ours-tesseract' THEN damage_rate END) AS ours_damage_rate,
                MAX(CASE WHEN reader = 'ours-tesseract' THEN function_share END) AS ours_function_share,
                MAX(CASE WHEN reader = 'ours-tesseract' THEN garbage_share END) AS ours_garbage_share,
                MAX(CASE WHEN reader = 'ours-tesseract' THEN known_words END) AS ours_known_words,
                MAX(CASE WHEN reader = 'as' THEN words END) AS as_words,
                MAX(CASE WHEN reader = 'as' AND COALESCE(loc_valid, TRUE) THEN est_cer END) AS as_est_cer,
                MAX(CASE WHEN reader = 'as' AND COALESCE(loc_valid, TRUE) THEN completeness END) AS as_completeness,
                MAX(CASE WHEN reader = 'as' THEN damage_rate END) AS as_damage_rate,
                MAX(CASE WHEN reader = 'as' THEN garbage_share END) AS as_garbage_share,
                MAX(CASE WHEN reader = 'ndnp' THEN est_cer END) AS ndnp_est_cer,
                MAX(CASE WHEN reader = 'ndnp' THEN findable_content END) AS ndnp_findable_content
            FROM (
                SELECT
                    r.*,
                    MAX(CASE WHEN r2.reader IN ('loc', 'loc-hist') THEN r2.words END) OVER (PARTITION BY r.doc_id) > 1 AS loc_valid
                FROM boinc_reader_rows r
                LEFT JOIN boinc_reader_rows r2 ON r.doc_id = r2.doc_id AND r2.reader IN ('loc', 'loc-hist')
            )
            GROUP BY doc_id
        )
        SELECT
            p.*,
            COALESCE(m.lccn, split_part(p.doc_id, '-', 1)) AS lccn,
            m.issue_date,
            m.edition,
            m.seq,
            m.batch,
            m.batch_version,
            m.manifest_lang,
            m.manifest_decision,
            m.manifest_damage_rate,
            m.item_url,
            CASE
                WHEN p.ours_est_cer IS NOT NULL AND p.loc_est_cer IS NOT NULL
                    THEN ROUND(p.loc_est_cer - p.ours_est_cer, 6)
                ELSE NULL
            END AS cer_gain_ours_vs_loc,
            CASE
                WHEN COALESCE(p.mean_word_conf, 0.75) < 0.55
                    OR COALESCE(p.ours_damage_rate, m.manifest_damage_rate, 0.15) >= 0.30
                    OR (COALESCE(p.ink_density, 0.20) < 0.12 AND COALESCE(p.contrast, 180.0) < 95.0)
                    OR COALESCE(p.contrast, 180.0) < 65.0
                    OR COALESCE(p.blur, 5000.0) < 900.0
                    OR COALESCE(p.legibility_ceiling, 0.80) < 0.55
                THEN 'severely_degraded'
                WHEN COALESCE(m.manifest_lang, p.boinc_lang, 'eng') IN ('deu', 'ger', 'srp', 'ces', 'cze', 'yid', 'zho', 'chi', 'haw')
                THEN 'multi_script_fraktur'
                WHEN COALESCE(p.columns, 1) >= 6 OR COALESCE(p.line_height_px, 34.0) < 26.0
                THEN 'dense_broadsheet'
                WHEN COALESCE(p.width, 2400) >= 2800
                    AND COALESCE(p.line_height_px, 34.0) >= 28.0
                    AND ((COALESCE(p.bleed_through, 0.08) >= 0.18 AND COALESCE(p.ink_density, 0.20) >= 0.25)
                         OR COALESCE(p.noise, 2.0) >= 14.5)
                THEN 'high_bleed_speckle'
                ELSE 'clean_standard'
            END AS router_regime,
            CASE
                WHEN COALESCE(p.legibility_ceiling, 0.80) < 0.40
                    OR COALESCE(p.ours_damage_rate, m.manifest_damage_rate, 0.15) >= 0.50
                    OR (
                        COALESCE(p.mean_word_conf, 0.75) < 0.55
                        AND (
                            (COALESCE(p.ink_density, 0.20) < 0.12 AND COALESCE(p.contrast, 180.0) < 95.0)
                            OR COALESCE(p.contrast, 180.0) < 65.0
                            OR COALESCE(p.blur, 5000.0) < 900.0
                        )
                    )
                THEN 'qwen-vlm'
                WHEN COALESCE(p.mean_word_conf, 0.75) < 0.55
                    OR COALESCE(p.ours_damage_rate, m.manifest_damage_rate, 0.15) >= 0.30
                    OR (COALESCE(p.ink_density, 0.20) < 0.12 AND COALESCE(p.contrast, 180.0) < 95.0)
                    OR COALESCE(p.contrast, 180.0) < 65.0
                    OR COALESCE(p.blur, 5000.0) < 900.0
                    OR COALESCE(p.legibility_ceiling, 0.80) < 0.55
                THEN 'surya'
                ELSE 'tesseract'
            END AS router_engine,
            CASE
                WHEN COALESCE(p.columns, 1) >= 6 OR COALESCE(p.line_height_px, 34.0) < 26.0 THEN 6
                ELSE 4
            END AS router_psm,
            CASE
                WHEN NOT (
                    COALESCE(p.mean_word_conf, 0.75) < 0.55
                    OR COALESCE(p.ours_damage_rate, m.manifest_damage_rate, 0.15) >= 0.30
                    OR (COALESCE(p.ink_density, 0.20) < 0.12 AND COALESCE(p.contrast, 180.0) < 95.0)
                    OR COALESCE(p.contrast, 180.0) < 65.0
                    OR COALESCE(p.blur, 5000.0) < 900.0
                    OR COALESCE(p.legibility_ceiling, 0.80) < 0.55
                )
                AND COALESCE(p.width, 2400) >= 2800
                AND COALESCE(p.line_height_px, 34.0) >= 28.0
                AND ((COALESCE(p.bleed_through, 0.08) >= 0.18 AND COALESCE(p.ink_density, 0.20) >= 0.25)
                     OR COALESCE(p.noise, 2.0) >= 14.5)
                THEN 0.5
                ELSE 1.0
            END AS router_scale,
            CASE
                WHEN COALESCE(p.columns, 1) >= 6 OR COALESCE(p.line_height_px, 34.0) < 26.0 THEN TRUE
                WHEN (
                    COALESCE(p.mean_word_conf, 0.75) < 0.55
                    OR COALESCE(p.ours_damage_rate, m.manifest_damage_rate, 0.15) >= 0.30
                    OR (COALESCE(p.ink_density, 0.20) < 0.12 AND COALESCE(p.contrast, 180.0) < 95.0)
                    OR COALESCE(p.contrast, 180.0) < 65.0
                    OR COALESCE(p.blur, 5000.0) < 900.0
                    OR COALESCE(p.legibility_ceiling, 0.80) < 0.55
                ) AND COALESCE(p.columns, 1) >= 2 THEN TRUE
                ELSE FALSE
            END AS router_slice_columns
        FROM page_agg p
        LEFT JOIN manifest_pages m USING (doc_id)
        """
    )

    # Full-corpus + BOINC Title-level Summary across all 3,664 LCCNs
    con.execute(
        """
        CREATE TABLE title_quality_summary AS
        WITH manifest_by_title AS (
            SELECT
                lccn,
                FIRST(manifest_lang) AS primary_lang,
                MIN(issue_date) AS min_issue_date,
                MAX(issue_date) AS max_issue_date,
                COUNT(*) AS manifest_damaged_pages,
                ROUND(AVG(manifest_damage_rate), 6) AS mean_manifest_damage_rate,
                ROUND(QUANTILE_CONT(manifest_damage_rate, 0.5), 6) AS median_manifest_damage_rate,
                ROUND(QUANTILE_CONT(manifest_damage_rate, 0.9), 6) AS p90_manifest_damage_rate,
                SUM(CASE WHEN manifest_damage_rate >= 0.25 THEN 1 ELSE 0 END) AS badly_damaged_pages
            FROM manifest_pages
            GROUP BY lccn
        ),
        boinc_by_title AS (
            SELECT
                lccn,
                COUNT(*) AS boinc_scored_pages,
                SUM(CASE WHEN hat_determined THEN 1 ELSE 0 END) AS boinc_hat_pages,
                ROUND(AVG(blur), 2) AS mean_blur,
                ROUND(AVG(contrast), 2) AS mean_contrast,
                ROUND(AVG(noise), 4) AS mean_noise,
                ROUND(AVG(bleed_through), 4) AS mean_bleed_through,
                ROUND(AVG(mean_word_conf), 4) AS mean_loc_word_conf,
                ROUND(AVG(loc_est_cer), 4) AS mean_loc_est_cer,
                ROUND(AVG(ours_est_cer), 4) AS mean_ours_est_cer,
                ROUND(AVG(cer_gain_ours_vs_loc), 4) AS mean_cer_gain_ours_vs_loc,
                ROUND(AVG(ours_cer_vs_loc), 4) AS mean_ours_cer_vs_loc,
                ROUND(AVG(ours_dice_vs_loc), 4) AS mean_ours_dice_vs_loc
            FROM page_quality_joined
            GROUP BY lccn
        )
        SELECT
            COALESCE(m.lccn, b.lccn) AS lccn,
            m.primary_lang,
            m.min_issue_date,
            m.max_issue_date,
            COALESCE(m.manifest_damaged_pages, 0) AS manifest_damaged_pages,
            COALESCE(m.badly_damaged_pages, 0) AS badly_damaged_pages,
            m.mean_manifest_damage_rate,
            m.median_manifest_damage_rate,
            m.p90_manifest_damage_rate,
            COALESCE(b.boinc_scored_pages, 0) AS boinc_scored_pages,
            COALESCE(b.boinc_hat_pages, 0) AS boinc_hat_pages,
            b.mean_blur,
            b.mean_contrast,
            b.mean_noise,
            b.mean_bleed_through,
            b.mean_loc_word_conf,
            b.mean_loc_est_cer,
            b.mean_ours_est_cer,
            b.mean_cer_gain_ours_vs_loc,
            b.mean_ours_cer_vs_loc,
            b.mean_ours_dice_vs_loc
        FROM manifest_by_title m
        FULL OUTER JOIN boinc_by_title b USING (lccn)
        ORDER BY COALESCE(m.manifest_damaged_pages, 0) DESC, lccn
        """
    )

    # Language-level summary combining full-corpus manifest + BOINC quality
    con.execute(
        """
        CREATE TABLE language_quality_summary AS
        WITH manifest_by_lang AS (
            SELECT
                manifest_lang AS lang,
                COUNT(DISTINCT lccn) AS manifest_titles,
                COUNT(*) AS manifest_damaged_pages,
                SUM(CASE WHEN manifest_damage_rate >= 0.25 THEN 1 ELSE 0 END) AS badly_damaged_pages,
                ROUND(AVG(manifest_damage_rate), 4) AS mean_damage_rate
            FROM manifest_pages
            GROUP BY manifest_lang
        ),
        boinc_by_lang AS (
            SELECT
                COALESCE(manifest_lang, boinc_lang) AS lang,
                COUNT(*) AS boinc_pages,
                SUM(CASE WHEN hat_determined THEN 1 ELSE 0 END) AS hat_pages,
                ROUND(AVG(loc_est_cer), 4) AS mean_loc_est_cer,
                ROUND(AVG(ours_est_cer), 4) AS mean_ours_est_cer,
                ROUND(AVG(cer_gain_ours_vs_loc), 4) AS mean_cer_gain_ours_vs_loc,
                ROUND(AVG(loc_findable_places), 4) AS mean_loc_findable_places,
                ROUND(AVG(ours_findable_places), 4) AS mean_ours_findable_places,
                ROUND(AVG(ours_dice_vs_loc), 4) AS mean_ours_dice_vs_loc
            FROM page_quality_joined
            GROUP BY COALESCE(manifest_lang, boinc_lang)
        )
        SELECT
            COALESCE(m.lang, b.lang) AS lang,
            COALESCE(m.manifest_titles, 0) AS manifest_titles,
            COALESCE(m.manifest_damaged_pages, 0) AS manifest_damaged_pages,
            COALESCE(m.badly_damaged_pages, 0) AS badly_damaged_pages,
            m.mean_damage_rate,
            COALESCE(b.boinc_pages, 0) AS boinc_pages,
            COALESCE(b.hat_pages, 0) AS hat_pages,
            b.mean_loc_est_cer,
            b.mean_ours_est_cer,
            b.mean_cer_gain_ours_vs_loc,
            b.mean_loc_findable_places,
            b.mean_ours_findable_places,
            b.mean_ours_dice_vs_loc
        FROM manifest_by_lang m
        FULL OUTER JOIN boinc_by_lang b USING (lang)
        ORDER BY COALESCE(m.manifest_damaged_pages, 0) DESC, COALESCE(b.boinc_pages, 0) DESC
        """
    )

    con.execute(
        """
        CREATE TABLE campaign_summary AS
        SELECT
            campaign,
            COUNT(DISTINCT doc_id) AS unique_pages,
            COUNT(*) AS reader_rows,
            SUM(CASE WHEN reader IN ('loc', 'loc-hist') AND COALESCE(hat_determined, FALSE) THEN 1 ELSE 0 END) AS hat_determined_pages,
            SUM(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN 1 ELSE 0 END) AS valid_loc_text_pages,
            ROUND(AVG(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN est_cer END), 4) AS mean_loc_est_cer,
            ROUND(AVG(CASE WHEN reader = 'ours-tesseract' THEN est_cer END), 4) AS mean_ours_est_cer,
            ROUND(AVG(CASE WHEN reader = 'as' THEN est_cer END), 4) AS mean_as_est_cer,
            ROUND(AVG(CASE WHEN reader IN ('loc', 'loc-hist') AND words > 1 THEN damage_rate END), 4) AS mean_loc_damage_rate,
            ROUND(AVG(CASE WHEN reader = 'ours-tesseract' THEN damage_rate END), 4) AS mean_ours_damage_rate,
            ROUND(AVG(CASE WHEN reader = 'as' THEN damage_rate END), 4) AS mean_as_damage_rate
        FROM raw_boinc_pages
        GROUP BY campaign
        ORDER BY campaign
        """
    )

    con.execute(
        """
        CREATE TABLE reader_engine_summary AS
        SELECT
            reader,
            COUNT(DISTINCT doc_id) AS pages,
            ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN est_cer END), 4) AS mean_est_cer,
            ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN completeness END), 4) AS mean_completeness,
            ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN precision END), 4) AS mean_precision,
            ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN findable_content END), 4) AS mean_findable_content,
            ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN findable_places END), 4) AS mean_findable_places,
            ROUND(AVG(cer_vs_loc), 4) AS mean_cer_vs_loc,
            ROUND(AVG(dice_vs_loc), 4) AS mean_dice_vs_loc,
            ROUND(AVG(CASE WHEN words > 1 THEN damage_rate END), 4) AS mean_damage_rate,
            ROUND(AVG(CASE WHEN words > 1 THEN function_share END), 4) AS mean_function_share,
            ROUND(AVG(CASE WHEN words > 1 THEN garbage_share END), 4) AS mean_garbage_share,
            ROUND(AVG(CASE WHEN words > 1 THEN known_words END), 1) AS mean_known_words
        FROM boinc_reader_rows
        GROUP BY reader
        ORDER BY pages DESC, reader
        """
    )

    # Per-reader-per-language rollup: page measures from the deduplicated reader rows, crop
    # measures from the raw crop rows (which carry no language, so language and group come
    # from any page row of the same doc_id). Rolled up by (language, reader) and by
    # (language, group, reader).
    con.execute(
        """
        CREATE TEMP TABLE doc_language AS
        SELECT doc_id, MIN(language) AS language, MIN("group") AS "group"
        FROM boinc_reader_rows
        GROUP BY doc_id
        """
    )
    for tbl, keys in [
        ("reader_language_summary", ["language"]),
        ("reader_language_group_summary", ["language", "group"]),
    ]:
        key_cols = ", ".join(f'"{k}"' for k in keys)
        con.execute(
            f"""
            CREATE TABLE {tbl} AS
            WITH page_stats AS (
                SELECT
                    {key_cols},
                    reader,
                    COUNT(*) AS pages,
                    ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN est_cer END), 4) AS mean_est_cer,
                    ROUND(MEDIAN(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN est_cer END)::DOUBLE, 4) AS median_est_cer,
                    ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN completeness END), 4) AS mean_completeness,
                    ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN precision END), 4) AS mean_precision,
                    ROUND(AVG(CASE WHEN loc_words > 1 OR COALESCE(hat_determined, FALSE) THEN findable_content END), 4) AS mean_findable_content,
                    ROUND(AVG(cer_vs_loc), 4) AS mean_cer_vs_loc,
                    ROUND(AVG(dice_vs_loc), 4) AS mean_dice_vs_loc,
                    SUM(CASE WHEN COALESCE(hat_determined, FALSE) THEN 1 ELSE 0 END) AS pages_hat_determined
                FROM boinc_reader_rows
                GROUP BY {key_cols}, reader
            ),
            crop_stats AS (
                SELECT
                    {", ".join(f'd."{k}"' for k in keys)},
                    c.reader,
                    COUNT(*) AS crops,
                    -- The collector writes an all-null score column as VARCHAR, so a campaign
                    -- with no VLM reading would otherwise abort AVG: cast first.
                    ROUND(AVG(TRY_CAST(c.hat_est_cer AS DOUBLE)), 4) AS mean_crop_hat_est_cer,
                    ROUND(AVG(TRY_CAST(c.cer_vs_vlm AS DOUBLE)), 4) AS mean_cer_vs_vlm,
                    ROUND(AVG(TRY_CAST(c.dice_vs_vlm AS DOUBLE)), 4) AS mean_dice_vs_vlm,
                    SUM(CASE WHEN c.vlm_text IS NOT NULL THEN 1 ELSE 0 END) AS crops_with_vlm
                FROM raw_boinc_crops c
                JOIN doc_language d USING (doc_id)
                GROUP BY {", ".join(f'd."{k}"' for k in keys)}, c.reader
            )
            SELECT
                {", ".join(f'COALESCE(p."{k}", c."{k}") AS "{k}"' for k in keys)},
                COALESCE(p.reader, c.reader) AS reader,
                COALESCE(p.pages, 0) AS pages,
                p.mean_est_cer,
                p.median_est_cer,
                p.mean_completeness,
                p.mean_precision,
                p.mean_findable_content,
                p.mean_cer_vs_loc,
                p.mean_dice_vs_loc,
                COALESCE(p.pages_hat_determined, 0) AS pages_hat_determined,
                COALESCE(c.crops, 0) AS crops,
                c.mean_crop_hat_est_cer,
                c.mean_cer_vs_vlm,
                c.mean_dice_vs_vlm,
                COALESCE(c.crops_with_vlm, 0) AS crops_with_vlm
            FROM page_stats p
            FULL OUTER JOIN crop_stats c
                ON {" AND ".join(f'p."{k}" IS NOT DISTINCT FROM c."{k}"' for k in keys)}
                AND p.reader = c.reader
            ORDER BY {key_cols}, reader
            """
        )

    # Pivot the per-language, per-reader rollup into one row per language so readers can be
    # compared side by side. The reader set comes from the summary itself (sorted); a reader
    # with only page or only crop measures leaves the other column null.
    comparison_readers = [
        r[0]
        for r in con.execute(
            "SELECT DISTINCT reader FROM reader_language_summary WHERE reader IS NOT NULL ORDER BY reader"
        ).fetchall()
    ]
    comparison_cols = []
    comparison_names = []
    for reader in comparison_readers:
        lit = reader.replace("'", "''")
        col = reader.replace("-", "_").replace('"', '""')
        for measure, suffix in (("mean_est_cer", "page_cer"), ("mean_crop_hat_est_cer", "crop_cer")):
            comparison_names.append(f'"{col}_{suffix}"')
            comparison_cols.append(
                f"MAX(CASE WHEN reader = '{lit}' THEN {measure} END) AS {comparison_names[-1]}"
            )
    # The counts are the language's distinct samples, not one reader's: readers can cover
    # different pages (a campaign stages only the readers it asked for), so the largest
    # per-reader count would understate a language whose readers share few pages. Crops
    # keep the per-reader policy of one count per campaign a crop appears in.
    # The collector names the crop column crop_id; the empty fallback view above names it id.
    crop_id_col = "crop_id" if any(
        r[0] == "crop_id" for r in con.execute("DESCRIBE raw_boinc_crops").fetchall()
    ) else "id"
    con.execute(
        "CREATE TABLE reader_language_comparison AS "
        "WITH readers AS (SELECT language, "
        + ", ".join(comparison_cols)
        + " FROM reader_language_summary GROUP BY language), "
        "page_counts AS (SELECT language, COUNT(DISTINCT doc_id) AS pages "
        "FROM boinc_reader_rows GROUP BY language), "
        "crop_counts AS (SELECT d.language, "
        f"COUNT(DISTINCT c.campaign || '|' || c.doc_id || '|' || c.{crop_id_col}) AS crops "
        "FROM raw_boinc_crops c JOIN doc_language d USING (doc_id) GROUP BY d.language) "
        "SELECT p.language, COALESCE(pc.pages, 0) AS pages, COALESCE(cc.crops, 0) AS crops, "
        + ", ".join(f"p.{name}" for name in comparison_names)
        + " FROM readers p "
        "LEFT JOIN page_counts pc ON pc.language IS NOT DISTINCT FROM p.language "
        "LEFT JOIN crop_counts cc ON cc.language IS NOT DISTINCT FROM p.language "
        "ORDER BY p.language"
    )

    con.execute(
        """
        CREATE TABLE router_regime_summary AS
        SELECT
            router_regime,
            router_engine,
            router_psm,
            router_scale,
            router_slice_columns,
            COUNT(*) AS pages,
            ROUND(AVG(loc_damage_rate), 4) AS mean_loc_damage_rate,
            ROUND(AVG(ours_damage_rate), 4) AS mean_ours_damage_rate,
            ROUND(AVG(loc_est_cer), 4) AS mean_loc_est_cer,
            ROUND(AVG(ours_est_cer), 4) AS mean_ours_est_cer,
            ROUND(AVG(mean_word_conf), 4) AS mean_word_conf,
            ROUND(AVG(bleed_through), 4) AS mean_bleed_through,
            ROUND(AVG(ink_density), 4) AS mean_ink_density,
            ROUND(AVG(columns), 2) AS mean_columns
        FROM page_quality_joined
        GROUP BY 1, 2, 3, 4, 5
        ORDER BY pages DESC
        """
    )

    con.execute(
        """
        CREATE TABLE next_wave_candidates AS
        WITH unscored AS (
            SELECT
                m.doc_id,
                m.lccn,
                m.issue_date,
                m.edition,
                m.seq,
                m.batch,
                m.batch_version,
                m.manifest_lang AS language,
                m.manifest_decision AS decision,
                m.manifest_damage_rate AS damage_rate,
                m.item_url,
                ROW_NUMBER() OVER (
                    PARTITION BY m.lccn
                    ORDER BY hash(m.doc_id)
                ) AS rn_in_title,
                ROW_NUMBER() OVER (
                    PARTITION BY m.manifest_lang
                    ORDER BY hash(m.doc_id)
                ) AS rn_in_lang
            FROM manifest_pages m
            LEFT JOIN page_quality_joined p USING (doc_id)
            WHERE p.doc_id IS NULL
        )
        SELECT
            doc_id,
            batch,
            batch_version,
            lccn,
            issue_date AS date,
            edition,
            seq,
            language,
            decision,
            damage_rate,
            item_url,
            CASE
                WHEN rn_in_title <= 2 THEN 'stratified-title-floor'
                WHEN rn_in_lang <= 200 THEN 'stratified-lang-floor'
                ELSE 'stratified-damage-tail'
            END AS "group"
        FROM unscored
        ORDER BY
            CASE WHEN rn_in_title <= 2 THEN 0 WHEN rn_in_lang <= 200 THEN 1 ELSE 2 END,
            rn_in_title,
            hash(doc_id)
        LIMIT 10240
        """
    )

    for tbl in [
        "boinc_reader_rows",
        "page_quality_joined",
        "title_quality_summary",
        "language_quality_summary",
        "campaign_summary",
        "reader_engine_summary",
        "reader_language_summary",
        "reader_language_group_summary",
        "reader_language_comparison",
        "router_regime_summary",
        "next_wave_candidates",
    ]:
        pq_out = out_dir / f"{tbl}.parquet"
        escaped_out = str(pq_out).replace("'", "''")
        con.execute(f"COPY {tbl} TO '{escaped_out}' (FORMAT PARQUET, COMPRESSION ZSTD)")

    next_wave_jsonl = out_dir / "next_wave8_stratified_10240.jsonl"
    escaped_jsonl = str(next_wave_jsonl).replace("'", "''")
    con.execute(f"COPY next_wave_candidates TO '{escaped_jsonl}' (FORMAT JSON)")

    if sqlite_path is not None:
        if sqlite_path.exists():
            sqlite_path.unlink()
        from decimal import Decimal
        scon = sqlite3.connect(sqlite_path)
        for tbl in [
            "boinc_reader_rows",
            "page_quality_joined",
            "title_quality_summary",
            "language_quality_summary",
            "campaign_summary",
            "reader_engine_summary",
            "reader_language_summary",
            "reader_language_group_summary",
            "reader_language_comparison",
            "router_regime_summary",
        ]:
            desc = con.execute(f"DESCRIBE {tbl}").fetchall()
            cols = [d[0] for d in desc]
            placeholders = ",".join(["?"] * len(cols))
            col_defs = []
            for name, dtype, *_ in desc:
                t = dtype.lower()
                sql_t = (
                    "INTEGER"
                    if ("int" in t or "bool" in t)
                    else ("REAL" if ("double" in t or "float" in t or "decimal" in t or "numeric" in t) else "TEXT")
                )
                col_defs.append(f'"{name}" {sql_t}')
            scon.execute(f"CREATE TABLE {tbl} ({', '.join(col_defs)})")
            raw_rows = con.execute(f"SELECT * FROM {tbl}").fetchall()
            rows_py = [
                tuple(float(v) if isinstance(v, Decimal) else v for v in r)
                for r in raw_rows
            ]
            if rows_py:
                scon.executemany(f"INSERT INTO {tbl} VALUES ({placeholders})", rows_py)
        scon.execute("CREATE INDEX idx_page_quality_doc_id ON page_quality_joined(doc_id)")
        scon.execute("CREATE INDEX idx_page_quality_lccn ON page_quality_joined(lccn)")
        scon.execute("CREATE INDEX idx_reader_rows_doc_id ON boinc_reader_rows(doc_id, reader)")
        scon.execute("CREATE INDEX idx_title_summary_lccn ON title_quality_summary(lccn)")
        scon.commit()
        scon.close()

    stats = {
        "campaigns": len(campaigns),
        "unique_boinc_pages": con.execute("SELECT COUNT(*) FROM page_quality_joined").fetchone()[0],
        "hat_determined_pages": con.execute(
            "SELECT COUNT(*) FROM page_quality_joined WHERE hat_determined"
        ).fetchone()[0],
        "boinc_reader_rows": con.execute("SELECT COUNT(*) FROM boinc_reader_rows").fetchone()[0],
        "titles_total": con.execute("SELECT COUNT(*) FROM title_quality_summary").fetchone()[0],
        "titles_with_boinc": con.execute(
            "SELECT COUNT(*) FROM title_quality_summary WHERE boinc_scored_pages > 0"
        ).fetchone()[0],
        "manifest_pages": con.execute("SELECT COUNT(*) FROM manifest_pages").fetchone()[0],
    }

    # Write Markdown summary
    md_lines = [
        "# Consolidated Full-Corpus Damage & BOINC Quality Assessment",
        "",
        f"- **Full-corpus damaged manifest pages (`usnm-ingest ocr-damage` v2):** `{stats['manifest_pages']:,}` across `{stats['titles_total']:,}` newspaper titles (`lccn`)",
        f"- **Deduplicated BOINC `usnm_quality` scored pages (Waves 1–7):** `{stats['unique_boinc_pages']:,}` (`{stats['boinc_reader_rows']:,}` reader rows across `{stats['campaigns']}` campaigns; `{stats['titles_with_boinc']:,}` titles sampled)",
        f"- **3+ reader pages with Three-Cornered Hat determined:** `{stats['hat_determined_pages']:,}`",
        "",
        "## Campaign Breakdown",
        "",
        "| Campaign | Unique Pages | Reader Rows | 3-Reader Hat Pages | Valid LoC Text Pages | Mean LoC Est. CER | Mean Ours-Tesseract Est. CER | Mean AS Est. CER | Mean LoC Damage | Mean Ours Damage | Mean AS Damage |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for r in con.execute("SELECT * FROM campaign_summary").fetchall():
        md_lines.append(
            f"| `{r[0]}` | {r[1]:,} | {r[2]:,} | {r[3]:,} | {r[4]:,} | "
            f"{r[5] if r[5] is not None else '-'} | {r[6] if r[6] is not None else '-'} | {r[7] if r[7] is not None else '-'} | "
            f"{r[8] if r[8] is not None else '-'} | {r[9] if r[9] is not None else '-'} | {r[10] if r[10] is not None else '-'} |"
        )

    md_lines.extend(
        [
            "",
            "## OCR Reader & Engine Summary (Deduplicated Across Campaigns)",
            "",
            "| Reader | Pages | Mean Est. CER | Completeness | Precision | Findable Content | Findable Places | CER vs LoC | Dice vs LoC | Damage Rate | Function Share | Garbage Share | Known Words |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for r in con.execute("SELECT * FROM reader_engine_summary").fetchall():
        md_lines.append(
            f"| `{r[0]}` | {r[1]:,} | "
            + " | ".join(str(x) if x is not None else "-" for x in r[2:])
            + " |"
        )

    md_lines.extend(
        [
            "",
            "## Damage-v1 OCR Router Regime Breakdown (`router_regime_summary`)",
            "",
            "| Regime | Engine | PSM | Scale | Slice Columns | Pages | Mean LoC Damage | Mean Ours Damage | Mean LoC CER | Mean Ours CER | Mean Word Conf | Mean Bleed | Mean Ink | Mean Cols |",
            "|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for r in con.execute("SELECT * FROM router_regime_summary").fetchall():
        md_lines.append(
            f"| `{r[0]}` | `{r[1]}` | {r[2]} | {r[3]} | `{r[4]}` | {r[5]:,} | "
            + " | ".join(str(x) if x is not None else "-" for x in r[6:])
            + " |"
        )

    md_lines.extend(
        [
            "",
            "## Language-Level Corpus Damage & BOINC Re-OCR Gain",
            "",
            "| Language | Titles | Damaged Pages (Manifest) | Badly Damaged (>=0.25) | Mean Damage Rate | BOINC Pages | Hat Pages | Mean LoC CER | Mean Ours CER | Mean CER Gain (LoC - Ours) | LoC Places | Ours Places |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for r in con.execute("SELECT * FROM language_quality_summary LIMIT 20").fetchall():
        md_lines.append(
            f"| `{r[0]}` | {r[1]:,} | {r[2]:,} | {r[3]:,} | "
            f"{r[4] if r[4] is not None else '-'} | {r[5]:,} | {r[6]:,} | "
            f"{r[7] if r[7] is not None else '-'} | {r[8] if r[8] is not None else '-'} | "
            f"{r[9] if r[9] is not None else '-'} | {r[10] if r[10] is not None else '-'} | "
            f"{r[11] if r[11] is not None else '-'} |"
        )

    md_lines.extend(
        [
            "",
            "## Reader Quality by Language (`reader_language_summary`)",
            "",
            "| Language | Reader | Pages | Mean Est. CER | Median Est. CER | Completeness | Precision | Findable Content | CER vs LoC | Dice vs LoC | Hat Pages | Crops | Crop Hat Est. CER | CER vs VLM | Dice vs VLM | Crops with VLM |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for r in con.execute("SELECT * FROM reader_language_summary").fetchall():
        md_lines.append(
            f"| `{r[0]}` | `{r[1]}` | {r[2]:,} | "
            + " | ".join(str(x) if x is not None else "-" for x in r[3:])
            + " |"
        )

    # Row layout: language, pages, crops, then (page_cer, crop_cer) per reader.
    comparison_rows = con.execute("SELECT * FROM reader_language_comparison").fetchall()
    md_lines.extend(["", "## Reader Comparison by Language (`reader_language_comparison`)"])
    for title, count_label, count_idx, offset in [
        ("Page-level estimated CER", "Pages", 1, 3),
        ("Crop-level hat estimated CER", "Crops", 2, 4),
    ]:
        md_lines.extend(
            [
                "",
                f"### {title}",
                "",
                "| Language | " + count_label + " | " + " | ".join(comparison_readers) + " |",
                "|---|---:|" + "---:|" * len(comparison_readers),
            ]
        )
        for r in comparison_rows:
            cells = [
                str(r[offset + 2 * i]) if r[offset + 2 * i] is not None else "-"
                for i in range(len(comparison_readers))
            ]
            md_lines.append(f"| `{r[0]}` | {r[count_idx]:,} | " + " | ".join(cells) + " |")

    (out_dir / "summary.md").write_text("\n".join(md_lines) + "\n", encoding="utf-8")
    return stats


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--quality-root",
        type=Path,
        required=True,
        help="Directory containing <campaign>/pages.parquet subdirectories",
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="Path to full-corpus manifest.parquet",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        required=True,
        help="Output directory for consolidated Parquet tables and summary.md",
    )
    parser.add_argument(
        "--sqlite-out",
        type=Path,
        default=None,
        help="Optional path to write indexed SQLite assessment database",
    )
    args = parser.parse_args()
    stats = build_database(args.quality_root, args.manifest, args.out_dir, args.sqlite_out)
    for k, v in stats.items():
        print(f"{k}: {v}")


if __name__ == "__main__":
    main()
