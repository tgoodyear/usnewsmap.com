-- Median damage per title, over the pages scored in any language.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  SELECT lccn, count(*) AS scored,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY damage_rate)::numeric, 3) AS damage_p50
  FROM quality.page_summary
  WHERE damage_rate IS NOT NULL
  GROUP BY 1
  ORDER BY 1
) TO STDOUT WITH CSV HEADER;
