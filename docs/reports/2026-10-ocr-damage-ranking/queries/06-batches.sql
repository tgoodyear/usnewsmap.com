-- Damage per digitisation batch: batches group the pages one vendor scanned and OCR'd.
SET default_transaction_read_only = on;
COPY (
  SELECT batch, count(*) AS pages, count(damage_rate) AS scored,
         count(*) FILTER (WHERE damage_rate > 0.25) AS bad,
         round(avg(damage_rate)::numeric, 3) AS mean_damage,
         count(DISTINCT lccn) AS titles, min(decade) AS first_decade, max(decade) AS last_decade
  FROM quality.page_summary
  GROUP BY 1
  ORDER BY 4 DESC, 1
) TO STDOUT WITH CSV HEADER;
