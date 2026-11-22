-- The English pilot's pages for Xiaomi-OCR-0 page mode (#1867, see docs/boinc/apps/
-- quality.md, "Why page mode, and how the pilot was chosen"): the worst tenth of English pages by qe-v1, leaving
-- out pages Tesseract's worst-first wave has fixed, stratified by decade and damage band in
-- proportion to the pool. Read-only; run on the replica as quality_reader. pilot_select.py
-- runs it and trims the draw; to run it by hand:
--
--   psql -X -q -v ON_ERROR_STOP=1 -v n=10000 -v seed=x08-eng-pilot-1 -v floor=30 \
--        -f pilot_selection.sql
--
-- English: `qe_loc_scores.language = 'eng'` with language_source 'page' (so a title fallback
-- or no language is out), a valid qe-v1 row of at least 50 words with an `lm_oov`. The tenths
-- are of all such pages, worst (highest lm_oov) first; the pool is the first.
-- Damage band, from quality.page_summary.damage_rate and #1401's cuts: worst over 0.25,
-- moderate over 0.10, low otherwise. A low-damage page in the worst qe tenth stays: where the
-- two measures disagree is worth seeing.
-- Fixed by Tesseract: the newest `ours-tesseract` output has a damage rate at least 0.02
-- below LoC's (quality_gain.py's measure and --min-gain). Pages it read without that gain, or
-- with no rate, stay in and carry tess_read = true; pages it has not read carry false.
-- Each stratum (decade, band) takes its share of :n, rounded up (so the draw is never short of
-- :n for rounding), at least :floor pages or all of a smaller stratum. The order inside a stratum is md5(seed || doc_id), so the draw repeats.
-- The total can pass :n by the rounding and the floors; pilot_select.py trims the largest
-- strata back.
BEGIN TRANSACTION READ ONLY;
COPY (
  WITH eng AS (
    SELECT doc_id, lm_oov
    FROM quality.qe_loc_scores
    WHERE qe_version = 'qe-v1' AND NOT invalid
      AND language = 'eng' AND language_source = 'page'
      AND NOT coalesce(no_words, false) AND words >= 50
      AND lm_oov IS NOT NULL
  ),
  ranked AS (
    SELECT doc_id, lm_oov, ntile(10) OVER (ORDER BY lm_oov DESC, doc_id) AS tenth
    FROM eng
  ),
  tess AS (
    SELECT DISTINCT ON (doc_id) doc_id, damage_rate
    FROM quality.ocr_outputs
    WHERE reader = 'ours-tesseract'
    ORDER BY doc_id, ocred_at DESC
  ),
  pool AS (
    SELECT r.doc_id, s.decade, r.lm_oov, s.damage_rate,
           CASE WHEN s.damage_rate > 0.25 THEN 'worst'
                WHEN s.damage_rate > 0.10 THEN 'moderate'
                ELSE 'low' END AS band,
           t.doc_id IS NOT NULL AS tess_read
    FROM ranked r
    JOIN quality.page_summary s USING (doc_id)
    LEFT JOIN tess t USING (doc_id)
    WHERE r.tenth = 1
      AND s.damage_rate IS NOT NULL AND s.decade IS NOT NULL
      AND (t.damage_rate IS NULL OR s.damage_rate - t.damage_rate < 0.02)
  ),
  alloc AS (
    SELECT decade, band, count(*) AS pages,
           least(count(*),
                 greatest(:floor, ceil(:n * count(*)::numeric / sum(count(*)) OVER ())))::int AS take
    FROM pool
    GROUP BY decade, band
  ),
  drawn AS (
    SELECT p.*, row_number() OVER (PARTITION BY decade, band
                                   ORDER BY md5(:'seed' || doc_id)) AS k
    FROM pool p
  )
  SELECT d.doc_id, d.decade, d.band, d.lm_oov, d.damage_rate, d.tess_read,
         a.pages AS stratum_pages, a.take, d.k
  FROM drawn d
  JOIN alloc a USING (decade, band)
  WHERE d.k <= a.take
  ORDER BY d.decade, d.band, d.k
) TO STDOUT WITH CSV HEADER;
COMMIT;
