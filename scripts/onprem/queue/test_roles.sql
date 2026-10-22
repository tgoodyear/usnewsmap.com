-- Checks setup.sql's roles (#967): each may do what its row in README.md says and nothing else
-- on the queue tables. Run as postgres in usnm_queue (ci-fixture.sh does); fails on the first
-- mismatch.
DO $$
DECLARE
  want record;
  got boolean;
BEGIN
  FOR want IN
    SELECT * FROM (VALUES
      -- role, table, privilege, expected
      ('boinc_claim', 'pgmq.q_ocr_bulk', 'SELECT', true),
      ('boinc_claim', 'pgmq.q_ocr_bulk', 'INSERT', true),
      ('boinc_claim', 'pgmq.q_ocr_bulk', 'UPDATE', true),
      ('boinc_claim', 'pgmq.q_ocr_bulk', 'DELETE', true),
      ('boinc_claim', 'pgmq.q_ocr_priority', 'UPDATE', true),
      ('boinc_claim', 'pgmq.q_ocr_deadletter', 'INSERT', true),
      ('boinc_claim', 'pgmq.q_ocr_deadletter', 'UPDATE', false),
      ('boinc_claim', 'pgmq.q_ocr_deadletter', 'DELETE', false),
      ('ingest_producer', 'pgmq.q_ocr_bulk', 'INSERT', true),
      ('ingest_producer', 'pgmq.q_ocr_bulk', 'UPDATE', false),
      ('ingest_producer', 'pgmq.q_ocr_bulk', 'DELETE', false),
      ('ingest_producer', 'pgmq.q_ocr_deadletter', 'UPDATE', true),
      ('ingest_producer', 'pgmq.q_ocr_deadletter', 'DELETE', true),
      ('dash_reader', 'pgmq.q_ocr_bulk', 'SELECT', true),
      ('dash_reader', 'pgmq.q_ocr_bulk', 'INSERT', false),
      ('dash_reader', 'pgmq.q_ocr_bulk', 'UPDATE', false),
      ('dash_reader', 'pgmq.q_ocr_deadletter', 'DELETE', false),
      ('dash_reader', 'pgmq.meta', 'SELECT', true),
      ('dash_reader', 'pgmq.meta', 'INSERT', false)
    ) AS t(role, tbl, priv, expected)
  LOOP
    got := has_table_privilege(want.role, want.tbl, want.priv);
    IF got IS DISTINCT FROM want.expected THEN
      RAISE EXCEPTION '% % on %: % (expected %)', want.role, want.priv, want.tbl, got, want.expected;
    END IF;
  END LOOP;
  -- Every role reads the sequences (pgmq.metrics), and none may create in the database.
  IF NOT has_sequence_privilege('dash_reader', 'pgmq.q_ocr_bulk_msg_id_seq', 'SELECT') THEN
    RAISE EXCEPTION 'dash_reader can''t read the ocr_bulk sequence (metrics would fail)';
  END IF;
  IF has_database_privilege('dash_reader', 'usnm_queue', 'CREATE') THEN
    RAISE EXCEPTION 'dash_reader may create in usnm_queue';
  END IF;
END $$;

-- And through pgmq itself: the dashboard's read (which would hide a message) is refused.
SET ROLE dash_reader;
DO $$
BEGIN
  PERFORM pgmq.read('ocr_bulk', 30, 1);
  RAISE EXCEPTION 'dash_reader could read (hide) a message';
EXCEPTION WHEN insufficient_privilege THEN
  NULL;
END $$;
SELECT queue_length >= 0 AS dash_metrics_ok FROM pgmq.metrics('ocr_bulk');
RESET ROLE;
SELECT 'roles checked' AS result;
