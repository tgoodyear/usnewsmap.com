-- Damage and agreement quantiles per detected language.
SET default_transaction_read_only = on;
SET work_mem = '512MB';
COPY (
  SELECT lang, count(*) AS scored,
         round(percentile_cont(0.1) WITHIN GROUP (ORDER BY damage_rate)::numeric, 3) AS damage_p10,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY damage_rate)::numeric, 3) AS damage_p50,
         round(percentile_cont(0.9) WITHIN GROUP (ORDER BY damage_rate)::numeric, 3) AS damage_p90,
         count(loc_as_dice) AS as_pair,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY loc_as_dice)::numeric, 3) AS dice_p50,
         count(*) FILTER (WHERE loc_as_matched_loc >= 0.5 AND loc_as_local_cer_sym IS NOT NULL) AS as_placed,
         round((percentile_cont(0.5) WITHIN GROUP (ORDER BY loc_as_local_cer_sym)
                FILTER (WHERE loc_as_matched_loc >= 0.5))::numeric, 3) AS local_cer_p50,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY loc_as_matched_loc)::numeric, 3) AS matched_p50
  FROM quality.page_summary
  WHERE damage_rate IS NOT NULL
  GROUP BY 1
  ORDER BY 2 DESC
) TO STDOUT WITH CSV HEADER;
