-- Pages to read again at full resolution (#968 A/B, 26 Nov 2026: Xiaomi-OCR-0 reads full-
-- resolution crops 2.6 points of CER better). One doc_id a line, worst damage first: a page
-- of language :lang with a VLM reading cut at half resolution (crop_res) and none at full. The
-- list feeds a listed re-read campaign (qwen_feed.py --pages FILE --language :lang --full-res);
-- docs/boinc/apps/quality.md, "Full-resolution crops". Read-only: quality_reader runs it.
--
--   psql "$DSN" -v lang=fra -v limit=2000 -tA -f half_res_reread.sql > fra-reread.txt
--
-- Held roots (--hold, the Xiaomi-OCR-0 campaigns) aren't loaded, so their pages come from
-- their own list files, not from here.
\if :{?limit}
\else
  \set limit 2000
\endif

SELECT s.doc_id
FROM quality.reader_scores s
JOIN quality.readers r USING (reader)
JOIN quality.pages p USING (doc_id)
LEFT JOIN quality.damage d USING (doc_id)
WHERE r.kind = 'vlm' AND p.lang = :'lang'
GROUP BY s.doc_id, d.damage_rate
HAVING bool_and(s.crop_res = 'half')
ORDER BY d.damage_rate DESC NULLS LAST, s.doc_id
LIMIT :limit;
