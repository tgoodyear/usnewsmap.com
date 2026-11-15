-- The quality measure (qe-v1, usnm_core::qe) of LoC's own OCR text, one row a page, for the
-- whole corpus: the `qe-loc-v1` text-only BOINC run (usnm_quality qe_text units). Run as
-- postgres on the primary, after taking a schema backup (pg_dump --schema-only -n quality);
-- safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f qe_loc_scores.sql
--
-- Append-only (the writer may only set `invalid`) and unique on (doc_id, qe_version) among valid
-- rows: a page is scored once for each measure version, and a later version is a new row beside it. Rows are written only from the units'
-- own reported values (`usnm-boinc quality qe-text-collect`, as quality_text_writer).

CREATE TABLE IF NOT EXISTS quality.qe_loc_scores (
  doc_id          text        NOT NULL CHECK (doc_id <> '' AND doc_id = btrim(doc_id)),
  -- The bulk OCR archive the page was read from.
  archive         text        NOT NULL,
  -- ISO 639-3 the page was measured in; null when it has no language.
  language        text,
  -- `page` (the page's detected language), `title_fallback` (detected `und`, the title's first
  -- language) or `none`.
  language_source text        NOT NULL CHECK (language_source IN ('page', 'title_fallback', 'none')),
  qe_version      text        NOT NULL,
  -- sha256 of ocr/qe/resources.lock.json, the pin the units were staged against.
  resources_lock  text        NOT NULL CHECK (resources_lock ~ '^[0-9a-f]{64}$'),
  oov             double precision,
  charlm_bpc      double precision,
  lm_oov          double precision,
  known3          bigint,
  no_words        boolean,
  words           bigint,
  scored_at       timestamptz NOT NULL DEFAULT now(),
  -- The BOINC host that computed the row and the app version it ran, so a host that fails a
  -- spot check can have all its rows marked invalid and its units sent again.
  host_id         bigint      NOT NULL,
  app_version     text        NOT NULL,
  -- Set only by `usnm-boinc quality qe-text-collect` when a spot check fails the row's host.
  invalid         boolean     NOT NULL DEFAULT false,
  -- Why there are no measures: no_language, no_resource_file, or no_words.
  null_reason     text CHECK (null_reason IN ('no_language', 'no_resource_file', 'no_words')),
  CHECK (btrim(app_version) <> '')
);
-- One valid row for a page and measure version. A row of a failed host is marked invalid and
-- the page's unit is scored again, so the new row sits beside it.
CREATE UNIQUE INDEX IF NOT EXISTS qe_loc_scores_valid
  ON quality.qe_loc_scores (doc_id, qe_version) WHERE NOT invalid;
CREATE INDEX IF NOT EXISTS qe_loc_scores_host ON quality.qe_loc_scores (host_id);
CREATE INDEX IF NOT EXISTS qe_loc_scores_archive ON quality.qe_loc_scores (archive);

GRANT SELECT ON quality.qe_loc_scores TO quality_loader, quality_text_writer, quality_reader;
GRANT INSERT ON quality.qe_loc_scores TO quality_text_writer;
GRANT UPDATE (invalid) ON quality.qe_loc_scores TO quality_text_writer;
