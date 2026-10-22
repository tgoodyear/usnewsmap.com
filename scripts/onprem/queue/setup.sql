-- The OCR work queues in PostgreSQL + pgmq (#967), in the usnm_queue database of the queue
-- container (home-infra scripts/lab/queue-ct.sh). Run as postgres in that container; safe to
-- run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_queue -f setup.sql
--
-- pgmq queue names become table names (pgmq.q_<name>), so they can't hold a hyphen: the
-- Azure names ocr-priority, ocr-bulk and ocr-deadletter are ocr_priority, ocr_bulk and
-- ocr_deadletter here (usnm-store's PgmqQueue maps them).
--
-- pgmq's functions run with the caller's rights, so what each role may do is set by its
-- grants on the queue tables. Every role logs in only with a client certificate whose CN is
-- the role (pg_hba: hostssl ... cert); none has a password.
--   boinc_claim      the BOINC claim and collect steps: read (hide for a lease), delete and
--                    send on the work queues (a page given back is sent again and the original
--                    deleted, so it goes to the back: pgmq reads in msg_id order), and send to
--                    ocr_deadletter.
--   ingest_producer  the seeder and the enqueue and requeue steps: send to every queue; read
--                    and delete ocr_deadletter (ocr-sync drains it).
--   dash_reader      the OCR dashboard and status: metrics and peek on every queue.

SELECT pgmq.create(q) FROM unnest(ARRAY['ocr_priority', 'ocr_bulk', 'ocr_deadletter']) AS q
WHERE NOT EXISTS (SELECT 1 FROM pgmq.meta WHERE queue_name = q);

DO $$
DECLARE r text;
BEGIN
  FOREACH r IN ARRAY ARRAY['boinc_claim', 'ingest_producer', 'dash_reader'] LOOP
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = r) THEN
      EXECUTE format('CREATE ROLE %I LOGIN', r);
    END IF;
    EXECUTE format('ALTER ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT PASSWORD NULL', r);
    EXECUTE format('GRANT CONNECT ON DATABASE usnm_queue TO %I', r);
    EXECUTE format('GRANT USAGE ON SCHEMA pgmq TO %I', r);
    -- metrics() reads the queue list.
    EXECUTE format('GRANT SELECT ON pgmq.meta TO %I', r);
    -- metrics() also reads each queue's message-id sequence.
    EXECUTE format('GRANT SELECT ON SEQUENCE pgmq.q_ocr_priority_msg_id_seq, pgmq.q_ocr_bulk_msg_id_seq, pgmq.q_ocr_deadletter_msg_id_seq TO %I', r);
  END LOOP;
END $$;

-- Nothing may change the database by default; each grant below is deliberate.
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON DATABASE usnm_queue FROM PUBLIC;

-- boinc_claim: lease, hand back and finish pages on the work queues; dead-letter what can't be done.
GRANT SELECT, INSERT, UPDATE, DELETE ON pgmq.q_ocr_priority, pgmq.q_ocr_bulk TO boinc_claim;
GRANT SELECT, INSERT ON pgmq.q_ocr_deadletter TO boinc_claim;
GRANT USAGE ON SEQUENCE pgmq.q_ocr_priority_msg_id_seq, pgmq.q_ocr_bulk_msg_id_seq,
  pgmq.q_ocr_deadletter_msg_id_seq TO boinc_claim;

-- ingest_producer: add pages to every queue; drain the dead letters.
GRANT SELECT, INSERT ON pgmq.q_ocr_priority, pgmq.q_ocr_bulk, pgmq.q_ocr_deadletter TO ingest_producer;
GRANT UPDATE, DELETE ON pgmq.q_ocr_deadletter TO ingest_producer;
GRANT USAGE ON SEQUENCE pgmq.q_ocr_priority_msg_id_seq, pgmq.q_ocr_bulk_msg_id_seq,
  pgmq.q_ocr_deadletter_msg_id_seq TO ingest_producer;

-- dash_reader: counts and peeks only.
GRANT SELECT ON pgmq.q_ocr_priority, pgmq.q_ocr_bulk, pgmq.q_ocr_deadletter TO dash_reader;
