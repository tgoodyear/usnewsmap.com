-- Qwen readings kept out of the quality tables until their unit is re-read (#1919). A row
-- names a `usnm_qwen` unit by its `job_id` (and its workunit `wu_id`), or one crop of it when
-- `crop_id` is set. `quality collect` reads the live rows (`released_at IS NULL`) as
-- `quality_reader` and builds no `vlm` row from an excluded unit or crop; the analysis reads
-- `quality.reader_scores_effective`, which leaves out the vlm readers' rows of a page with a
-- live exclusion. Run as postgres on the primary (it reaches the replica a second later);
-- safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f excluded_qwen_readings.sql
--
-- Add a row as postgres:
--
--   INSERT INTO quality.excluded_qwen_readings (job_id, wu_id, doc_id, reason, created_by)
--   VALUES ('<64 hex>', 54155, '<doc_id>', 'why', 'who');
--
-- Release one by setting `released_at` (rows are never deleted, so the history stays; a
-- trigger refuses a delete and any change but the release):
--
--   UPDATE quality.excluded_qwen_readings SET released_at = now()
--   WHERE job_id = '<64 hex>' AND crop_id IS NULL AND released_at IS NULL;
--
-- `crop_id` is the id as the unit's readings.json has it (`<doc_id>:<crop>` in a bundle).
-- `doc_id` is the page the unit read (a bundle unit has one row for each of its pages): the view needs it to find the page-level rows, because
-- reader_scores keys on the page and has no Qwen job. Without it the exclusion still keeps the
-- readings out of `quality collect`, but the view cannot match a row.

CREATE TABLE IF NOT EXISTS quality.excluded_qwen_readings (
  exclusion_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  job_id       text NOT NULL CHECK (job_id ~ '^[0-9a-f]{64}$'),
  wu_id        bigint NOT NULL,
  -- The page the unit read, when known.
  doc_id       text CHECK (doc_id <> '' AND doc_id = btrim(doc_id)),
  -- NULL excludes the whole unit.
  crop_id      text CHECK (crop_id <> '' AND crop_id = btrim(crop_id)),
  reason       text NOT NULL CHECK (btrim(reason) <> ''),
  created_at   timestamptz NOT NULL DEFAULT now(),
  created_by   text NOT NULL CHECK (btrim(created_by) <> ''),
  released_at  timestamptz CHECK (released_at >= created_at)
);
-- One live exclusion for a unit or crop (and page, for a bundle, which has a row for each page
-- it read), so adding it again changes nothing.
CREATE UNIQUE INDEX IF NOT EXISTS excluded_qwen_readings_live
  ON quality.excluded_qwen_readings (job_id, coalesce(crop_id, ''), coalesce(doc_id, ''))
  WHERE released_at IS NULL;
CREATE INDEX IF NOT EXISTS excluded_qwen_readings_doc
  ON quality.excluded_qwen_readings (doc_id) WHERE released_at IS NULL;

-- History is kept: a row is never deleted, and the only change to one is its release.
CREATE OR REPLACE FUNCTION quality.excluded_qwen_readings_guard() RETURNS trigger
LANGUAGE plpgsql AS $f$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'excluded_qwen_readings rows are released (released_at), never deleted';
  END IF;
  IF OLD.released_at IS NOT NULL THEN
    RAISE EXCEPTION 'excluded_qwen_readings: exclusion % is already released', OLD.exclusion_id;
  END IF;
  IF (NEW.exclusion_id, NEW.job_id, NEW.wu_id, NEW.doc_id, NEW.crop_id, NEW.reason, NEW.created_at,
      NEW.created_by) IS DISTINCT FROM
     (OLD.exclusion_id, OLD.job_id, OLD.wu_id, OLD.doc_id, OLD.crop_id, OLD.reason, OLD.created_at,
      OLD.created_by) THEN
    RAISE EXCEPTION 'excluded_qwen_readings: only released_at may change';
  END IF;
  RETURN NEW;
END
$f$;
DROP TRIGGER IF EXISTS excluded_qwen_readings_guard ON quality.excluded_qwen_readings;
CREATE TRIGGER excluded_qwen_readings_guard BEFORE UPDATE OR DELETE ON quality.excluded_qwen_readings
  FOR EACH ROW EXECUTE FUNCTION quality.excluded_qwen_readings_guard();

-- reader_scores without the vlm readers' rows of a page whose Qwen readings are excluded.
-- Every other reader's row, and every row of any other page, is as stored. The analysis
-- (quality_gain.py, per-cell statistics) reads this view, not the table.
CREATE OR REPLACE VIEW quality.reader_scores_effective AS
SELECT r.*
FROM quality.reader_scores r
WHERE NOT EXISTS (
  SELECT FROM quality.readers rd, quality.excluded_qwen_readings x
  WHERE rd.reader = r.reader AND rd.kind = 'vlm'
    AND x.doc_id = r.doc_id AND x.released_at IS NULL
);

GRANT SELECT ON quality.excluded_qwen_readings, quality.reader_scores_effective
  TO quality_loader, quality_text_writer, quality_reader;
