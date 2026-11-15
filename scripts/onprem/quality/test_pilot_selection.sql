-- A fixture for pilot_selection.sql (#1998), loaded by ci-check.sh into its own database after
-- setup.sql and qe_loc_scores.sql. Run as postgres.
--
-- 400 valid English pages pilot-0001 .. pilot-0400, lm_oov = the number, so the worst tenth is
-- 361 to 400. Decade: 1890 for even numbers, 1900 for odd. Damage rate: 0.40 (worst) when the
-- number is a multiple of 3, 0.20 (moderate) when one over, 0.05 (low) otherwise.
-- Pages 401 to 406 have a lm_oov above every valid page and a reason to be left out.
-- Tesseract: 361 and 365 are fixed (a drop of 0.10 and 0.04 in the newest output) and so out;
-- 362 has a drop of 0.01, 363 an output with no rate and 364 a newest output with no drop
-- after an older one with a big one, so all three stay and carry tess_read.
INSERT INTO quality.campaigns (campaign, source) VALUES ('pilot-damage', 'damage')
  ON CONFLICT DO NOTHING;
INSERT INTO quality.pages (doc_id, batch, batch_version, lccn, date, edition, seq)
SELECT 'pilot-' || lpad(i::text, 4, '0'), 'xx_pilot', 1, 'sn0',
       make_date(CASE WHEN i % 2 = 0 THEN 1890 ELSE 1900 END + i % 10, 1, 1), 1, 1
FROM generate_series(1, 406) AS i;
INSERT INTO quality.damage (doc_id, campaign, damage_rate)
SELECT 'pilot-' || lpad(i::text, 4, '0'), 'pilot-damage',
       CASE i % 3 WHEN 0 THEN 0.40 WHEN 1 THEN 0.20 ELSE 0.05 END
FROM generate_series(1, 406) AS i;
INSERT INTO quality.qe_loc_scores
  (doc_id, archive, language, language_source, qe_version, resources_lock, lm_oov, no_words, words,
   host_id, app_version, invalid)
SELECT 'pilot-' || lpad(i::text, 4, '0'), 'xx_pilot_ver01.tar.bz2',
       CASE WHEN i = 402 THEN NULL WHEN i = 406 THEN 'ger' ELSE 'eng' END,
       CASE i WHEN 401 THEN 'title_fallback' WHEN 402 THEN 'none' ELSE 'page' END,
       CASE WHEN i = 405 THEN 'qe-v0' ELSE 'qe-v1' END,
       repeat('a', 64),
       CASE WHEN i <= 400 THEN i ELSE 1000 + i END, false,
       CASE WHEN i = 403 THEN 10 ELSE 100 END,
       1, 'ci', i = 404
FROM generate_series(1, 406) AS i;
SELECT quality.refresh_summary();
INSERT INTO quality.ocr_outputs (doc_id, reader, ocred_at, damage_rate)
SELECT 'pilot-' || lpad(i::text, 4, '0'), 'ours-tesseract', t, rate
FROM (VALUES
  (361, '2026-11-01'::timestamptz, 0.10::real),
  (362, '2026-11-01', 0.04),
  (363, '2026-11-01', NULL),
  (364, '2026-10-01', 0.05),
  (364, '2026-11-01', 0.39),
  (365, '2026-10-01', 0.39),
  (365, '2026-11-01', 0.01)
) AS v(i, t, rate);
SELECT quality.refresh_summary();
