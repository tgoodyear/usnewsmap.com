-- What our OCR did to the damage score, paired per page, by LoC's damage on that page. Each
-- page counts once per engine profile, with its newest run. The pages were not sampled at
-- random, and both scores are the damage metric, not measured accuracy.
SET default_transaction_read_only = on;
COPY (
  WITH o AS (
    SELECT DISTINCT ON (doc_id, profile) doc_id, profile, damage_rate, text_chars
    FROM (SELECT *, substring(engine FROM 'profile=([^ ]+)') AS profile FROM quality.ocr_outputs) x
    WHERE damage_rate IS NOT NULL
    ORDER BY doc_id, profile, ocred_at DESC
  )
  SELECT o.profile, d.lang,
         width_bucket(d.damage_rate, ARRAY[0, 0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5]::real[]) AS bin,
         min(d.damage_rate)::numeric(4, 3) AS bin_low,
         count(*) AS pages,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY d.damage_rate)::numeric, 3) AS loc_p50,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY o.damage_rate)::numeric, 3) AS ours_p50,
         round(avg((o.damage_rate < d.damage_rate)::int)::numeric, 3) AS lower_share,
         round(avg(o.damage_rate - d.damage_rate)::numeric, 3) AS mean_change
  FROM o JOIN quality.damage d USING (doc_id)
  WHERE d.damage_rate IS NOT NULL
  GROUP BY 1, 2, 3
  ORDER BY 1, 2, 3
) TO STDOUT WITH CSV HEADER;
