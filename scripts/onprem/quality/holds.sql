-- Holds on single readings of single pages (#1956). The release reads this table at release time,
-- as quality_reader, and leaves each held reading out as a revocation would; it needs no file
-- in the curated lake. Run as postgres on the primary (it reaches the replica a second later);
-- safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f holds.sql
--
-- A hold names a page and the reading to withdraw: the stored `ocr_engine` tag (in full, or the
-- short key `<family>/<version> <profile>`), or the `node` (`boinc-<12 hex of the job id>`), or
-- both. One naming neither would take every reading of the page, so it is refused. A page's
-- other readings stay in the race. Revocations that select by node, run, time window or host are
-- still files under `curated/ocr-local/revoked/`, and so are page lists made from a comparison
-- (docs/ocr/revoke-page-list.md); this table is for the odd page.
--
-- Rows are added and removed as postgres (the readers have SELECT only). Removing a row gives
-- the reading back to the next release; published indexes keep what they have until a rebuild.

CREATE TABLE IF NOT EXISTS quality.boinc_page_holds (
  hold_id    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  doc_id     text NOT NULL CHECK (doc_id <> '' AND doc_id = btrim(doc_id)),
  -- The published `node` of the reading, when the hold is by node.
  node       text CHECK (node <> '' AND node = btrim(node)),
  -- The stored `ocr_engine` tag, in full or by its short key, when the hold is by engine.
  engine     text CHECK (engine <> '' AND engine = btrim(engine)),
  reason     text NOT NULL CHECK (btrim(reason) <> ''),
  created_at timestamptz NOT NULL DEFAULT now(),
  created_by text NOT NULL CHECK (btrim(created_by) <> ''),
  CHECK (node IS NOT NULL OR engine IS NOT NULL)
);
-- One hold for a page and reading, so adding it again changes nothing.
CREATE UNIQUE INDEX IF NOT EXISTS boinc_page_holds_reading
  ON quality.boinc_page_holds (doc_id, coalesce(engine, ''), coalesce(node, ''));

GRANT SELECT ON quality.boinc_page_holds TO quality_loader, quality_text_writer, quality_reader;
