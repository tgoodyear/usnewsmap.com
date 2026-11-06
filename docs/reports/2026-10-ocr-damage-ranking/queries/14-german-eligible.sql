-- The strata the pilot draws from, for spread and coverage figures in the report.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  SELECT CASE WHEN s.damage_rate > 0.25 THEN 'bad' ELSE 'moderate_high' END AS tier, s.lccn, s.decade,
         count(*) AS eligible_pages, round(avg(s.damage_rate)::numeric, 3) AS mean_damage
  FROM quality.page_summary s JOIN quality.damage d USING (doc_id)
  WHERE s.lang = 'ger' AND s.decision = 'detected' AND s.decade < 1920 AND s.ocr_runs = 0
    AND s.damage_rate > 0.15 AND d.head + d.near >= 50 AND d.words >= 500
  GROUP BY 1, 2, 3
  ORDER BY 1, 2, 3
) TO STDOUT WITH CSV HEADER;
