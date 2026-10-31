-- Row counts and missing-data rates for every signal, over quality.page_summary.
SET default_transaction_read_only = on;
COPY (
  SELECT count(*) AS pages,
         count(*) FILTER (WHERE decision = 'no_text') AS no_text,
         count(*) FILTER (WHERE decision = 'und') AS und,
         count(*) FILTER (WHERE decision = 'mixed') AS mixed,
         count(*) FILTER (WHERE decision = 'detected') AS detected,
         count(damage_rate) AS damage_scored,
         count(loc_as_dice) AS as_pair,
         count(loc_as_local_cer_sym) AS as_local_cer,
         count(*) FILTER (WHERE loc_as_matched_loc >= 0.5 AND loc_as_local_cer_sym IS NOT NULL) AS as_placed_half,
         count(loc_wc_mean) AS loc_word_conf,
         count(loc_est_cer) AS hat_loc,
         count(*) FILTER (WHERE has_image_features) AS image_features,
         count(*) FILTER (WHERE ocr_runs > 0) AS our_ocr,
         count(DISTINCT lccn) AS titles,
         count(DISTINCT batch) AS batches,
         min(date) AS first_date,
         max(date) AS last_date
  FROM quality.page_summary
) TO STDOUT WITH CSV HEADER;
