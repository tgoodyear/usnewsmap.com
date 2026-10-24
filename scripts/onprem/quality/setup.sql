-- The OCR-quality results database (#962): usnm_quality in the queue container CT231
-- (home-infra scripts/lab/queue-ct.sh --quality-db), beside usnm_queue and separate from it.
-- One row per page in quality.pages, every signal about a page in its own table keyed by
-- doc_id, and quality.page_summary joining them, one row a page. Run as postgres in that
-- container; safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f setup.sql
--
-- Roles log in only with a client certificate whose CN is the role (ADR-0025); none has a
-- password. What each may do is its row in README.md, checked by test_roles.sql:
--   quality_loader       the loaders (load.py): writes pages, damage, campaigns, readers,
--                        unit results, OCR outputs and staging; refreshes page_summary.
--   quality_text_writer  the text-only comparisons (LoC against American Stories, #962):
--                        writes text_compare and its own campaign rows; reads the rest.
--   quality_reader       dashboards and the OCR ranking queries (rank.sql): reads only.

CREATE SCHEMA IF NOT EXISTS quality;

-- Every page of the corpus, from the damage audit's per-batch files (one per batch version).
CREATE TABLE IF NOT EXISTS quality.pages (
  doc_id        text PRIMARY KEY,
  batch         text NOT NULL,
  batch_version smallint NOT NULL,
  lccn          text NOT NULL,
  date          date NOT NULL,
  edition       smallint NOT NULL,
  seq           smallint NOT NULL,
  lang          text,
  decade        smallint GENERATED ALWAYS AS ((extract(year FROM date)::int / 10 * 10)::smallint) STORED
);
CREATE INDEX IF NOT EXISTS pages_lccn_decade ON quality.pages (lccn, decade);
CREATE INDEX IF NOT EXISTS pages_batch ON quality.pages (batch, batch_version);

-- What produced a set of rows: a BOINC quality campaign (v1-wave9), the damage audit
-- (damage-v2), a text comparison (as-compare-...), an OCR export (ocr-local).
CREATE TABLE IF NOT EXISTS quality.campaigns (
  campaign  text PRIMARY KEY CHECK (campaign ~ '^[a-z0-9][a-z0-9.-]{0,63}$'),
  source    text NOT NULL CHECK (source IN ('boinc-quality', 'damage', 'text-compare', 'ocr')),
  engine    text,
  notes     text,
  loaded_at timestamptz NOT NULL DEFAULT now()
);

-- The texts we hold of a page, by the reader names quality units use.
CREATE TABLE IF NOT EXISTS quality.readers (
  reader      text PRIMARY KEY CHECK (reader ~ '^[a-z0-9-]{1,32}$'),
  kind        text NOT NULL CHECK (kind IN ('loc', 'american-stories', 'ours', 'vlm')),
  description text
);
INSERT INTO quality.readers (reader, kind, description) VALUES
  ('loc-hist', 'loc', 'LoC''s OCR text as the curated store holds it'),
  ('loc', 'loc', 'LoC''s OCR text from its ALTO'),
  ('as', 'american-stories', 'American Stories (Dell et al.) page text'),
  ('ours-tesseract', 'ours', 'our Tesseract OCR (curated ocr-local)'),
  ('ours-surya', 'ours', 'our Surya OCR'),
  ('ours-ndlocr', 'ours', 'our NDLOCR-Lite OCR'),
  ('ours-paddle', 'ours', 'our PaddleOCR'),
  ('qwen', 'vlm', 'usnm_qwen crop readings')
ON CONFLICT (reader) DO NOTHING;

-- The damage score (text only), crates/usnm-core damage v2: curated/audit/damage/v2.
CREATE TABLE IF NOT EXISTS quality.damage (
  doc_id         text PRIMARY KEY REFERENCES quality.pages,
  campaign       text NOT NULL REFERENCES quality.campaigns,
  text_status    text,
  decision       text,
  lang           text,
  script         text,
  damage_rate    real,
  function_share real,
  garbage_share  real,
  words          integer,
  head           integer,
  near           integer
);
CREATE INDEX IF NOT EXISTS damage_rate ON quality.damage (damage_rate DESC NULLS LAST);

-- A usnm_quality unit's page-level result (quality collect's pages.parquet, or the wave
-- runner's collected parts): the scan's features, the ALTO's layout, the hat's fit.
CREATE TABLE IF NOT EXISTS quality.unit_pages (
  campaign           text NOT NULL REFERENCES quality.campaigns,
  doc_id             text NOT NULL,
  job_id             text NOT NULL,
  grp                text,
  language           text,
  outcome            text,
  width              integer,
  height             integer,
  skew_deg           real,
  blur               real,
  contrast           real,
  noise              real,
  bleed_through      real,
  ink_density        real,
  blocks             integer,
  lines              integer,
  line_words         integer,
  columns            integer,
  line_height_px     real,
  mean_word_conf     real,
  consensus_words    integer,
  hat_determined     boolean,
  hat_residual       real,
  legibility_ceiling real,
  readers            smallint,
  has_alto           boolean,
  total_ms           integer,
  PRIMARY KEY (campaign, doc_id)
);
CREATE INDEX IF NOT EXISTS unit_pages_doc ON quality.unit_pages (doc_id);

-- Per reader on that page: the hat's estimate and the consensus measures, and the raw
-- pairwise agreement with every other reader.
CREATE TABLE IF NOT EXISTS quality.reader_scores (
  campaign         text NOT NULL REFERENCES quality.campaigns,
  doc_id           text NOT NULL,
  reader           text NOT NULL REFERENCES quality.readers,
  source           text,
  chars            integer,
  words            integer,
  est_cer          real,
  est_cer_raw      real,
  completeness     real,
  precision        real,
  findable_content real,
  findable_places  real,
  cer_vs_loc       real,
  wer_vs_loc       real,
  dice_vs_loc      real,
  pairs            jsonb,
  PRIMARY KEY (campaign, doc_id, reader)
);
CREATE INDEX IF NOT EXISTS reader_scores_doc ON quality.reader_scores (doc_id);

-- A text-only comparison of two readers of a page (no image): LoC against American Stories
-- over the corpus, and any later reader pair. Rates as quality::Pair: a is the hypothesis.
CREATE TABLE IF NOT EXISTS quality.text_compare (
  campaign    text NOT NULL REFERENCES quality.campaigns,
  doc_id      text NOT NULL,
  reader_a    text NOT NULL REFERENCES quality.readers,
  reader_b    text NOT NULL REFERENCES quality.readers,
  cer_a       real,
  cer_b       real,
  cer_sym     real,
  wer_a       real,
  wer_b       real,
  dice        real,
  chars_a     integer,
  chars_b     integer,
  words_a     integer,
  words_b     integer,
  computed_at timestamptz,
  PRIMARY KEY (campaign, doc_id, reader_a, reader_b),
  CHECK (reader_a <> reader_b)
);
CREATE INDEX IF NOT EXISTS text_compare_doc ON quality.text_compare (doc_id);

-- Our OCR of a page (curated ocr-local), one row per published run.
CREATE TABLE IF NOT EXISTS quality.ocr_outputs (
  doc_id      text NOT NULL,
  reader      text NOT NULL REFERENCES quality.readers,
  engine      text,
  source      text,
  ocred_at    timestamptz NOT NULL,
  text_chars  integer,
  damage_rate real,
  PRIMARY KEY (doc_id, reader, ocred_at)
);

-- Where a page's quality unit came from: which campaign staged it and with which image
-- (the keep set, an OCR fetch, the full image pass) and ALTO (empty, LoC bulk, ours).
CREATE TABLE IF NOT EXISTS quality.staging (
  campaign     text NOT NULL REFERENCES quality.campaigns,
  doc_id       text NOT NULL,
  image_source text NOT NULL,
  alto_source  text NOT NULL,
  staged_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (campaign, doc_id)
);
CREATE INDEX IF NOT EXISTS staging_doc ON quality.staging (doc_id);

-- One row a page, joining every signal: the newest unit result, LoC's own estimate there,
-- the best of our readers, and the newest LoC-against-American-Stories comparison.
-- Kept on a rerun; a change to its definition needs DROP MATERIALIZED VIEW first.
CREATE MATERIALIZED VIEW IF NOT EXISTS quality.page_summary AS
WITH unit AS (
  SELECT DISTINCT ON (u.doc_id) u.*
  FROM quality.unit_pages u JOIN quality.campaigns c USING (campaign)
  WHERE u.outcome = 'ok'
  ORDER BY u.doc_id, c.loaded_at DESC
), loc AS (
  SELECT r.doc_id, r.est_cer, r.completeness
  FROM unit JOIN quality.reader_scores r USING (campaign, doc_id)
  WHERE r.reader IN ('loc', 'loc-hist')
), ours AS (
  SELECT r.doc_id, min(r.est_cer) AS best_ours_est_cer
  FROM unit JOIN quality.reader_scores r USING (campaign, doc_id)
  WHERE r.reader LIKE 'ours-%'
  GROUP BY r.doc_id
), las AS (
  SELECT DISTINCT ON (t.doc_id) t.doc_id, t.cer_sym, t.dice
  FROM quality.text_compare t JOIN quality.campaigns c USING (campaign)
  WHERE t.reader_a IN ('loc', 'loc-hist') AND t.reader_b = 'as'
  ORDER BY t.doc_id, c.loaded_at DESC
), ocr AS (
  SELECT doc_id, count(*) AS ocr_runs FROM quality.ocr_outputs GROUP BY doc_id
)
SELECT p.doc_id, p.batch, p.batch_version, p.lccn, p.date, p.decade, p.lang,
       d.damage_rate, d.garbage_share, d.function_share, d.words AS loc_words, d.decision,
       unit.campaign AS unit_campaign, unit.readers AS unit_readers,
       unit.skew_deg, unit.blur, unit.contrast, unit.noise, unit.bleed_through, unit.ink_density,
       unit.mean_word_conf, unit.hat_determined, unit.legibility_ceiling,
       loc.est_cer AS loc_est_cer, loc.completeness AS loc_completeness,
       ours.best_ours_est_cer,
       las.cer_sym AS loc_as_cer_sym, las.dice AS loc_as_dice,
       coalesce(ocr.ocr_runs, 0) AS ocr_runs,
       unit.doc_id IS NOT NULL AS has_image_features
FROM quality.pages p
LEFT JOIN quality.damage d USING (doc_id)
LEFT JOIN unit USING (doc_id)
LEFT JOIN loc USING (doc_id)
LEFT JOIN ours USING (doc_id)
LEFT JOIN las USING (doc_id)
LEFT JOIN ocr USING (doc_id);
CREATE UNIQUE INDEX IF NOT EXISTS page_summary_doc ON quality.page_summary (doc_id);

-- REFRESH needs the view's owner; the loader gets this function instead of ownership.
CREATE OR REPLACE FUNCTION quality.refresh_summary() RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = quality, pg_temp
AS $$ REFRESH MATERIALIZED VIEW CONCURRENTLY quality.page_summary $$;
REVOKE ALL ON FUNCTION quality.refresh_summary() FROM PUBLIC;

DO $$
DECLARE r text;
BEGIN
  FOREACH r IN ARRAY ARRAY['quality_loader', 'quality_text_writer', 'quality_reader'] LOOP
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = r) THEN
      EXECUTE format('CREATE ROLE %I LOGIN', r);
    END IF;
    EXECUTE format('ALTER ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT PASSWORD NULL', r);
    EXECUTE format('GRANT CONNECT ON DATABASE usnm_quality TO %I', r);
    EXECUTE format('GRANT USAGE ON SCHEMA quality TO %I', r);
    EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA quality TO %I', r);
  END LOOP;
END $$;

-- Nothing may change the database by default; each grant below is deliberate.
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON DATABASE usnm_quality FROM PUBLIC;

-- quality_loader: upserts through a temporary table (COPY, then INSERT ... ON CONFLICT).
GRANT TEMPORARY ON DATABASE usnm_quality TO quality_loader;
GRANT INSERT, UPDATE, DELETE ON quality.pages, quality.damage, quality.campaigns, quality.unit_pages,
  quality.reader_scores, quality.ocr_outputs, quality.staging TO quality_loader;
GRANT INSERT ON quality.readers TO quality_loader;
GRANT EXECUTE ON FUNCTION quality.refresh_summary() TO quality_loader;

-- quality_text_writer: its comparisons and the campaign rows that name them.
GRANT TEMPORARY ON DATABASE usnm_quality TO quality_text_writer;
GRANT INSERT, UPDATE ON quality.text_compare TO quality_text_writer;
GRANT INSERT ON quality.campaigns TO quality_text_writer;

-- quality_reader: SELECT only (above).
