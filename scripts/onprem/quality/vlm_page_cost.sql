-- What reading a page with a vision-language model cost (#1998): one row for each checked
-- result that read a page in page mode, from `usnm-boinc qwen page-collect`'s
-- `vlm_page_cost.jsonl`. Run as postgres on the primary, after taking a schema backup
-- (pg_dump --schema-only -n quality); safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f vlm_page_cost.sql
--
-- Append-only: the loader (load.py xiaomi) may insert and never change a row, and a result is
-- recorded once for a page. `gpu_seconds` is the result's (load_ms + engine_ms) times the
-- page's share of the unit's completion tokens, divided by 1000. The BOINC columns are not in
-- a result; they are filled from the BOINC provenance (provenance.sql) when it is joined.

CREATE TABLE IF NOT EXISTS quality.vlm_page_cost (
  doc_id            text NOT NULL CHECK (doc_id <> '' AND doc_id = btrim(doc_id)),
  campaign          text NOT NULL,
  job_id            text NOT NULL,
  -- BOINC's name of the result.
  result_name       text NOT NULL CHECK (result_name <> ''),
  plan_class        text,
  gpu               text,
  gpu_mem_total_mib bigint,
  slots             integer,
  ctx_size          integer,
  load_ms           bigint NOT NULL CHECK (load_ms >= 0),
  engine_ms         bigint NOT NULL CHECK (engine_ms >= 0),
  page_share        double precision NOT NULL CHECK (page_share >= 0 AND page_share <= 1),
  gpu_seconds       double precision NOT NULL CHECK (gpu_seconds >= 0),
  regions           integer NOT NULL,
  tokens            bigint NOT NULL,
  chars_out         integer NOT NULL,
  gpu_mem_peak_mib  bigint,
  outcome           text NOT NULL,
  -- An agreeing pair includes the result.
  validated         boolean NOT NULL,
  -- The result is the unit's canonical result.
  canonical         boolean NOT NULL,
  result_id         bigint,
  host_id           bigint,
  app_version       text,
  platform          text,
  loaded_at         timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (doc_id, result_name)
);
CREATE INDEX IF NOT EXISTS vlm_page_cost_campaign ON quality.vlm_page_cost (campaign);
CREATE INDEX IF NOT EXISTS vlm_page_cost_plan ON quality.vlm_page_cost (plan_class);

GRANT SELECT ON quality.vlm_page_cost TO quality_loader, quality_text_writer, quality_reader;
GRANT INSERT ON quality.vlm_page_cost TO quality_loader;
