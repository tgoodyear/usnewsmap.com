-- LoC's own word confidence by the OCR software its ALTO names: the scale differs per engine.
SET default_transaction_read_only = on;
COPY (
  SELECT coalesce(software, 'not stated') AS software, count(*) AS pages,
         round(avg(wc_mean)::numeric, 3) AS mean_wc, count(*) FILTER (WHERE wc_mean IS NULL) AS no_wc
  FROM quality.loc_alto
  GROUP BY 1
  ORDER BY 2 DESC
) TO STDOUT WITH CSV HEADER;
