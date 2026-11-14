-- Migration for holds.sql (#1956): a hold that matches no stored page stops the release, unless
-- the row says it is for a reading that isn't stored yet. Run as postgres on the primary after
-- holds.sql (safe to run again):
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f holds_allow_zero_match.sql
--
-- Rows are written with `usnm-ingest holds add`, which refuses a row that matches nothing unless
-- given --allow-zero-match; existing rows get false.

ALTER TABLE quality.boinc_page_holds
  ADD COLUMN IF NOT EXISTS allow_zero_match boolean NOT NULL DEFAULT false;
