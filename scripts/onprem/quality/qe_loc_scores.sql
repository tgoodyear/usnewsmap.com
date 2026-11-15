-- The quality measure (qe-v1, usnm_core::qe) of LoC's own OCR text, one row a page, for the
-- whole corpus: the `qe-loc-v1` text-only BOINC run (usnm_quality qe_text units). Run as
-- postgres on the primary, after taking a schema backup (pg_dump --schema-only -n quality);
-- safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f qe_loc_scores.sql
--
-- Append-only and unique on (doc_id, qe_version): a page is scored once for each measure
-- version, and a later version is a new row beside it. Rows are written only from the units'
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
  -- Why there are no measures: no_language, no_resource_file, or no_words.
  null_reason     text CHECK (null_reason IN ('no_language', 'no_resource_file', 'no_words')),
  PRIMARY KEY (doc_id, qe_version)
);
CREATE INDEX IF NOT EXISTS qe_loc_scores_archive ON quality.qe_loc_scores (archive);

GRANT SELECT ON quality.qe_loc_scores TO quality_loader, quality_text_writer, quality_reader;
GRANT INSERT ON quality.qe_loc_scores TO quality_text_writer;
