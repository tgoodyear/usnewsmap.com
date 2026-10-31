-- On the three-reader pages: LoC's hat estimate by damage band, and rank correlation of the
-- other signals with it. The pages were chosen worst-first by earlier campaigns, so this is
-- not a sample of the corpus.
SET default_transaction_read_only = on;
COPY (
  WITH h AS (
    SELECT lang, damage_rate, loc_est_cer, loc_matched, loc_as_dice, loc_as_local_cer_sym, loc_as_matched_loc, loc_wc_mean
    FROM quality.page_summary
    WHERE loc_est_cer IS NOT NULL AND damage_rate IS NOT NULL
  ), r AS (
    SELECT rank() OVER (ORDER BY damage_rate) AS rd, rank() OVER (ORDER BY loc_est_cer) AS rh FROM h
  )
  SELECT 'all' AS grp, count(*) AS pages,
         round(corr(rd, rh)::numeric, 3) AS spearman_damage_hat,
         NULL::numeric AS hat_p50, NULL::numeric AS damage_p50, NULL::numeric AS matched_p50
  FROM r
  UNION ALL
  SELECT CASE WHEN damage_rate <= 0.1 THEN '1 fine' WHEN damage_rate <= 0.25 THEN '2 moderate' ELSE '3 bad' END,
         count(*), NULL,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY loc_est_cer)::numeric, 3),
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY damage_rate)::numeric, 3),
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY loc_matched)::numeric, 3)
  FROM h GROUP BY 1
  ORDER BY 1
) TO STDOUT WITH CSV HEADER;
