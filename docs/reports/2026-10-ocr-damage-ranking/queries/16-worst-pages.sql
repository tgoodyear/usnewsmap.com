-- The 100 worst pages of each detected language with 1,000 or more scored pages: highest
-- damage rate with head + near at least 50, not yet read by our OCR.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  WITH r AS (
    SELECT s.lang, s.doc_id, s.lccn, s.date, s.decade, s.batch, s.damage_rate,
           d.head + d.near AS head_plus_near, d.words, s.garbage_share, s.function_share, s.loc_as_dice,
           row_number() OVER (PARTITION BY s.lang ORDER BY s.damage_rate DESC, s.doc_id) AS rank
    FROM quality.page_summary s JOIN quality.damage d USING (doc_id)
    WHERE s.damage_rate IS NOT NULL AND d.head + d.near >= 50 AND s.ocr_runs = 0
      AND s.lang IN (SELECT lang FROM quality.damage WHERE damage_rate IS NOT NULL GROUP BY 1 HAVING count(*) >= 1000)
  )
  SELECT lang, rank, doc_id, lccn, date, decade, batch, round(damage_rate::numeric, 4) AS damage_rate,
         head_plus_near, words, round(garbage_share::numeric, 4) AS garbage_share,
         round(function_share::numeric, 4) AS function_share, round(loc_as_dice::numeric, 3) AS loc_as_dice
  FROM r
  WHERE rank <= 100
  ORDER BY 1, 2
) TO STDOUT WITH CSV HEADER;
