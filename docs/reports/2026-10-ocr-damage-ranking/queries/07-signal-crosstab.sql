-- Do the signals point the same way? Damage band against the American Stories agreement class,
-- per detected language.
SET default_transaction_read_only = on;
COPY (
  SELECT lang,
         CASE WHEN damage_rate <= 0.1 THEN '1 fine' WHEN damage_rate <= 0.25 THEN '2 moderate'
              ELSE '3 bad' END AS damage_band,
         CASE WHEN loc_as_dice IS NULL THEN '0 no pair'
              WHEN loc_as_matched_loc IS NULL OR loc_as_matched_loc < 0.5 OR loc_as_local_cer_sym IS NULL THEN '1 under half placed'
              WHEN loc_as_local_cer_sym <= 0.1 THEN '2 placed, local_cer <= 0.1'
              WHEN loc_as_local_cer_sym <= 0.2 THEN '3 placed, local_cer 0.1-0.2'
              ELSE '4 placed, local_cer > 0.2' END AS agreement_class,
         count(*) AS pages,
         round(sum(loc_as_dice::float8)::numeric, 3) AS sum_dice
  FROM quality.page_summary
  WHERE damage_rate IS NOT NULL
  GROUP BY 1, 2, 3
  ORDER BY 1, 2, 3
) TO STDOUT WITH CSV HEADER;
