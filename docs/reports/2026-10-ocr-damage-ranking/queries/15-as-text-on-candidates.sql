-- For pages where LoC's text is in a damage band, how American Stories' text of the same page
-- scores on the same metric. "as_band" needs AS to hold at least half LoC's characters
-- (len_ratio is AS's characters over LoC's), so a sliver of a page doesn't count as usable.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  WITH c AS (
    SELECT s.lang,
           CASE WHEN s.damage_rate <= 0.1 THEN '1 fine' WHEN s.damage_rate <= 0.25 THEN '2 moderate' ELSE '3 bad' END AS loc_band,
           s.ocr_runs = 0 AND d.head + d.near >= 50 AS eligible,
           t.doc_id IS NOT NULL AS has_pair, t.len_ratio, t.damage_rate_b
    FROM quality.page_summary s
    JOIN quality.damage d USING (doc_id)
    LEFT JOIN quality.text_compare t ON t.doc_id = s.doc_id AND t.campaign = 'text-agreement-v1'
    WHERE s.damage_rate IS NOT NULL
  )
  SELECT lang, loc_band, eligible,
         CASE WHEN NOT has_pair THEN '0 no AS text'
              WHEN len_ratio IS NULL OR len_ratio < 0.5 THEN '1 AS under half of LoC'
              WHEN damage_rate_b IS NULL THEN '2 AS not scored'
              WHEN damage_rate_b <= 0.1 THEN '3 AS fine'
              WHEN damage_rate_b <= 0.25 THEN '4 AS moderate'
              ELSE '5 AS bad' END AS as_band,
         count(*) AS pages
  FROM c
  GROUP BY 1, 2, 3, 4
  ORDER BY 1, 2, 3, 4
) TO STDOUT WITH CSV HEADER;
