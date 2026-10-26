-- The queries that rank pages for OCR and for the full image pass (#962), over
-- quality.page_summary. Read-only: quality_reader runs them. Each is a separate statement;
-- psql -v limit=1000 -f rank.sql prints all three.
\if :{?limit}
\else
  \set limit 1000
\endif

-- 1. OCR priority: pages we haven't OCR'd, worst LoC text first. The estimate is the best
--    signal a page has: the hat's est_cer for LoC (three readers or more), else LoC's
--    passage-matched disagreement with American Stories (agreement v1's local_cer_sym,
--    which counts both texts' errors, so half of it; the whole-page cer_sym mostly measures
--    article order and text one reader lacks, #962), but only where at least half of
--    LoC's text found a place in American Stories' (matched_loc); a low local rate over
--    a sliver of the page says nothing about the rest. Else the mean of the damage score
--    and LoC's own doubt (1 - mean word confidence from its ALTO), so a damaged page LoC
--    was also unsure of comes first; without the ALTO's confidence, the damage score alone.
--    Pages LoC has no text for are left to the damage audit.
SELECT doc_id, lccn, decade, lang,
       coalesce(loc_est_cer, CASE WHEN loc_as_matched_loc >= 0.5 THEN loc_as_local_cer_sym / 2 END,
                (damage_rate + coalesce(1 - loc_wc_mean, damage_rate)) / 2) AS loc_badness,
       CASE WHEN loc_est_cer IS NOT NULL THEN 'hat'
            WHEN loc_as_matched_loc >= 0.5 AND loc_as_local_cer_sym IS NOT NULL THEN 'loc-vs-as'
            WHEN loc_wc_mean IS NOT NULL THEN 'damage+loc-wc'
            ELSE 'damage' END AS basis
FROM quality.page_summary
WHERE ocr_runs = 0
  AND coalesce(loc_est_cer, CASE WHEN loc_as_matched_loc >= 0.5 THEN loc_as_local_cer_sym END, damage_rate) IS NOT NULL
ORDER BY loc_badness DESC, doc_id
LIMIT :limit;

-- 2. The full image pass's order: pages with no image features yet, worst first (damage and
--    LoC's own doubt, as above),
--    taken round-robin over (title, decade) strata so every stratum gets its worst pages
--    early instead of the worst title taking the whole budget.
SELECT doc_id, lccn, decade, damage_rate, stratum_rank
FROM (
  SELECT doc_id, lccn, decade, damage_rate,
         row_number() OVER (PARTITION BY lccn, decade
                            ORDER BY (damage_rate + coalesce(1 - loc_wc_mean, damage_rate)) / 2 DESC NULLS LAST,
                                     doc_id) AS stratum_rank
  FROM quality.page_summary
  WHERE NOT has_image_features
) s
ORDER BY stratum_rank, damage_rate DESC NULLS LAST, doc_id
LIMIT :limit;

-- 3. Titles by how much OCR would help them: pages, mean damage, share badly damaged
--    (damage_rate above 0.25, BADLY_DAMAGED in crates/usnm-core damage), and coverage.
SELECT lccn, count(*) AS pages,
       round(avg(damage_rate)::numeric, 3) AS mean_damage,
       round(avg((damage_rate > 0.25)::int)::numeric, 3) AS badly_damaged_share,
       sum((ocr_runs > 0)::int) AS ocr_pages,
       sum(has_image_features::int) AS imaged_pages
FROM quality.page_summary
GROUP BY lccn
ORDER BY sum((damage_rate > 0.25)::int) DESC NULLS LAST, lccn
LIMIT :limit;
