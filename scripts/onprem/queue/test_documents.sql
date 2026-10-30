\set ON_ERROR_STOP on
SET ROLE ingest_producer;
INSERT INTO usnm_state.documents(container,pk,id,doc)
VALUES ('schema-test','one','one','{"id":"one","value":1}')
ON CONFLICT(container,pk,id) DO NOTHING;
RESET ROLE;
SET ROLE dash_reader;
DO $$
BEGIN
    IF (SELECT COUNT(*) FROM usnm_state.documents WHERE container='schema-test') != 1 THEN
        RAISE EXCEPTION 'dash_reader cannot read documents';
    END IF;
    BEGIN
        UPDATE usnm_state.documents SET doc='{"id":"one","value":2}' WHERE container='schema-test';
        RAISE EXCEPTION 'dash_reader can write documents';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
END $$;
RESET ROLE;
DELETE FROM usnm_state.documents WHERE container='schema-test';
