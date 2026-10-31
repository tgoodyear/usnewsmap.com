-- How the German candidate pool narrows, one exclusion at a time. Each row counts the pages
-- left after its step, in the two tiers of 13-german-pilot.sql (any page outside them is in
-- neither count). The order of the steps matches the report's table.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  WITH g AS (
    SELECT s.damage_rate, s.decision, s.decade, s.ocr_runs, d.head + d.near AS den, d.words
    FROM quality.page_summary s JOIN quality.damage d USING (doc_id)
    WHERE s.lang = 'ger'
  ), t AS (
    SELECT *, CASE WHEN damage_rate > 0.25 THEN 'bad' WHEN damage_rate > 0.15 THEN 'moderate_high' END AS tier
    FROM g
  ), steps(step, label) AS (
    VALUES (1, 'detected or mixed German'), (2, 'detected only'), (3, 'printed before 1920'),
           (4, 'no output of our OCR yet'), (5, 'head + near at least 50'), (6, 'at least 500 words')
  )
  SELECT step, label,
         count(*) FILTER (WHERE tier = 'bad') AS bad,
         count(*) FILTER (WHERE tier = 'moderate_high') AS moderate_high
  FROM steps LEFT JOIN t ON (step < 2 OR decision = 'detected')
                        AND (step < 3 OR decade < 1920)
                        AND (step < 4 OR ocr_runs = 0)
                        AND (step < 5 OR den >= 50)
                        AND (step < 6 OR words >= 500)
  GROUP BY 1, 2
  ORDER BY 1
) TO STDOUT WITH CSV HEADER;
