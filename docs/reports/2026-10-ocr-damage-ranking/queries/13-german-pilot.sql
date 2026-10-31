-- German Fraktur pilot candidates: 3,750 bad pages (damage above 0.25) and 1,250 pages of the
-- upper moderate band (above 0.15 to 0.25). Our earlier German OCR runs hold few pages in that
-- band, so its gain is unmeasured. Each tier is taken round-robin over (title, decade), as
-- rank.sql's image pass does. Inside a stratum, pages come worst first, one page per issue
-- before any issue gives a second, so the worst issues lead and no issue fills a stratum.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  WITH e AS (
    SELECT s.doc_id, s.lccn, s.date, p.edition, p.seq, s.decade, s.batch, s.batch_version,
           s.damage_rate, d.head + d.near AS den, d.words, s.garbage_share, s.function_share,
           s.loc_wc_mean, s.loc_as_dice, s.has_image_features,
           CASE WHEN s.damage_rate > 0.25 THEN 'bad' ELSE 'moderate_high' END AS tier
    FROM quality.page_summary s
    JOIN quality.damage d USING (doc_id)
    JOIN quality.pages p USING (doc_id)
    WHERE s.lang = 'ger' AND s.decision = 'detected' AND s.decade < 1920 AND s.ocr_runs = 0
      AND s.damage_rate > 0.15 AND d.head + d.near >= 50 AND d.words >= 500
  ), by_issue AS (
    SELECT *, row_number() OVER (PARTITION BY tier, lccn, date, edition ORDER BY damage_rate DESC, doc_id) AS issue_rank
    FROM e
  ), by_stratum AS (
    SELECT *, row_number() OVER (PARTITION BY tier, lccn, decade ORDER BY issue_rank, damage_rate DESC, doc_id) AS stratum_rank
    FROM by_issue
  ), ranked AS (
    SELECT *, row_number() OVER (PARTITION BY tier ORDER BY stratum_rank, damage_rate DESC, doc_id) AS tier_rank
    FROM by_stratum
  )
  SELECT tier, tier_rank, stratum_rank, doc_id, lccn, date, edition, seq, decade, batch, batch_version,
         round(damage_rate::numeric, 4) AS damage_rate, den AS head_plus_near, words,
         round(garbage_share::numeric, 4) AS garbage_share, round(function_share::numeric, 4) AS function_share,
         round(loc_wc_mean::numeric, 3) AS loc_wc_mean, round(loc_as_dice::numeric, 3) AS loc_as_dice,
         has_image_features
  FROM ranked
  WHERE (tier = 'bad' AND tier_rank <= 3750) OR (tier = 'moderate_high' AND tier_rank <= 1250)
  ORDER BY CASE tier WHEN 'bad' THEN 0 ELSE 1 END, tier_rank
) TO STDOUT WITH CSV HEADER;
