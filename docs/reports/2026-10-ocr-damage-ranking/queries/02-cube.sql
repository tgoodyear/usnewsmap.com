-- One row per (detected language, decision, title, decade): the counts every language, title
-- and decade table is summed from. Bands use crates/usnm-core damage's constants (DAMAGED 0.1,
-- BADLY_DAMAGED 0.25). "den" is head + near, the occurrences the page's damage rate is a share
-- of; below 50 a rate is treated as weak evidence.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  WITH p AS (
    SELECT s.*, d.head + d.near AS den, d.words,
           s.loc_as_matched_loc >= 0.5 AND s.loc_as_local_cer_sym IS NOT NULL AS placed
    FROM quality.page_summary s JOIN quality.damage d USING (doc_id)
  )
  SELECT coalesce(lang, 'none') AS lang, decision, lccn, decade,
         count(*) AS pages,
         count(*) FILTER (WHERE words < 20) AS under_20_words,
         count(*) FILTER (WHERE decision = 'und' AND words >= 100) AS und_100_words,
         count(damage_rate) AS scored,
         count(*) FILTER (WHERE damage_rate IS NOT NULL AND den < 50) AS weak,
         count(*) FILTER (WHERE damage_rate <= 0.1) AS fine,
         count(*) FILTER (WHERE damage_rate > 0.1 AND damage_rate <= 0.25) AS moderate,
         count(*) FILTER (WHERE damage_rate > 0.25) AS bad,
         count(*) FILTER (WHERE damage_rate > 0.5) AS severe,
         count(*) FILTER (WHERE damage_rate > 0.25 AND den < 50) AS bad_weak,
         count(*) FILTER (WHERE damage_rate > 0.1 AND damage_rate <= 0.25 AND den < 50) AS moderate_weak,
         round(sum(damage_rate::float8)::numeric, 3) AS sum_damage,
         count(*) FILTER (WHERE ocr_runs > 0) AS ocr_done,
         count(*) FILTER (WHERE damage_rate > 0.25 AND ocr_runs > 0) AS bad_ocr_done,
         count(*) FILTER (WHERE damage_rate > 0.25 AND den >= 50 AND ocr_runs = 0) AS strong_todo,
         count(*) FILTER (WHERE damage_rate > 0.25 AND den >= 50 AND ocr_runs = 0 AND loc_as_dice IS NOT NULL) AS strong_todo_with_as,
         count(*) FILTER (WHERE damage_rate > 0.25 AND den >= 50 AND ocr_runs = 0 AND loc_as_dice >= 0.6) AS strong_todo_as_dice_60,
         count(*) FILTER (WHERE damage_rate > 0.1 AND damage_rate <= 0.25 AND den >= 50 AND ocr_runs = 0) AS moderate_todo,
         count(loc_as_dice) AS as_pair,
         count(*) FILTER (WHERE placed) AS as_placed,
         round(sum(loc_as_dice::float8)::numeric, 3) AS sum_dice,
         round((sum(loc_as_local_cer_sym::float8) FILTER (WHERE placed))::numeric, 3) AS sum_local_cer_placed,
         count(loc_wc_mean) AS wc_pages,
         round(sum(loc_wc_mean::float8)::numeric, 3) AS sum_wc,
         count(loc_est_cer) AS hat_pages
  FROM p
  GROUP BY 1, 2, 3, 4
  ORDER BY 1, 2, 3, 4
) TO STDOUT WITH CSV HEADER;
