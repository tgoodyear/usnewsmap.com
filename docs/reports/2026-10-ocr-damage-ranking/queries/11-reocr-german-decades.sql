-- The same pairing for German pages read with frak2021, per decade of print.
SET default_transaction_read_only = on;
COPY (
  WITH o AS (
    SELECT DISTINCT ON (doc_id) doc_id, damage_rate
    FROM quality.ocr_outputs
    WHERE engine LIKE '%profile=frak2021 %' AND damage_rate IS NOT NULL
    ORDER BY doc_id, ocred_at DESC
  )
  SELECT p.decade, count(*) AS pages,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY d.damage_rate)::numeric, 3) AS loc_p50,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY o.damage_rate)::numeric, 3) AS frak2021_p50,
         round(avg((o.damage_rate < d.damage_rate)::int)::numeric, 3) AS lower_share,
         count(*) FILTER (WHERE d.damage_rate > 0.25) AS bad_pages,
         round((percentile_cont(0.5) WITHIN GROUP (ORDER BY o.damage_rate) FILTER (WHERE d.damage_rate > 0.25))::numeric, 3) AS frak2021_p50_bad
  FROM o JOIN quality.damage d USING (doc_id) JOIN quality.pages p USING (doc_id)
  WHERE d.damage_rate IS NOT NULL AND d.lang = 'ger'
  GROUP BY 1
  ORDER BY 1
) TO STDOUT WITH CSV HEADER;
