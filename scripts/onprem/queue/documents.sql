-- Apply as the database owner. Import under stopped writers; retain source ETags.
CREATE SCHEMA IF NOT EXISTS usnm_state;
REVOKE ALL ON SCHEMA usnm_state FROM PUBLIC;
CREATE SEQUENCE IF NOT EXISTS usnm_state.document_versions;
CREATE TABLE IF NOT EXISTS usnm_state.documents (
    container text NOT NULL,
    pk text NOT NULL,
    id text NOT NULL,
    doc jsonb NOT NULL,
    etag text NOT NULL DEFAULT ('pg-' || nextval('usnm_state.document_versions')::text),
    PRIMARY KEY (container, pk, id),
    CHECK (jsonb_typeof(doc) = 'object' AND doc->>'id' IS NOT NULL AND doc->>'id' = id)
);
GRANT USAGE ON SCHEMA usnm_state TO boinc_claim, ingest_producer, dash_reader;
GRANT SELECT, INSERT, UPDATE ON usnm_state.documents TO boinc_claim, ingest_producer;
GRANT USAGE ON SEQUENCE usnm_state.document_versions TO boinc_claim, ingest_producer;
GRANT SELECT ON usnm_state.documents TO dash_reader;
