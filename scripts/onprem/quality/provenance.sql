-- Which BOINC host read a published page (#1956). One row per `route=boinc` reading in
-- `curated/ocr-local/pages` (a page and its `node`), built from the BOINC database while the
-- workunit and result rows still exist (db_purge removes them 30 days after a workunit is done). Run as postgres in the
-- queue container; safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f provenance.sql
--
-- Fill it with `boinc_provenance.py` (resolves the published pages against a read-only export
-- of the BOINC database; its docstring has the queries) and `provenance_load.sql` (the upsert).
--
-- `status` says how far the page resolved; only `resolved` rows carry a host. A later build
-- never replaces a `resolved` row with a less complete one. A host whose CPU vendor is empty or
-- unknown stays `resolved` with a NULL `family`: the host is what a hold needs.
--
-- The release reads this table at release time, as quality_reader (docs/ocr/revoke-page-list.md).

CREATE TABLE IF NOT EXISTS quality.boinc_page_provenance (
  doc_id              text NOT NULL,
  -- The published `node`, `boinc-<first 12 hex of the job id>`: with doc_id, one reading.
  node                text NOT NULL,
  job_id              text,
  wu_id               integer,
  canonical_result_id integer,
  hostid              integer,
  app_version_id      integer,
  platform            text,
  -- 'arm64' or 'amd64' from the host row (os_name, p_vendor), never from the platform; NULL
  -- when the host row names no vendor, or one this does not know.
  family              text CHECK (family IN ('arm64', 'amd64')),
  -- 'wu_name' (usnm_<job id>), 'bundle_member' (usnm_ocrb_<bundle>, by its member list), either
  -- with '+dup_wu_same_host' when the job also ran in a second workunit on the same host.
  resolved_via        text,
  resolved_at         timestamptz NOT NULL,
  status              text NOT NULL,
  -- The 12 hex of the engine image the published row names, and the full digest where a bundle
  -- request still lists the job.
  image12             text NOT NULL,
  image_digest        text,
  -- Hosts behind a job that ran in two workunits with different canonical hosts.
  candidate_host_ids  integer[],
  PRIMARY KEY (doc_id, node)
);
-- A resolved row needs its host and workunit, not its family (#1956 follow-up). The constraint
-- is replaced so a table made by the first version of this file picks the change up; the old
-- one was the table's unnamed CHECK.
ALTER TABLE quality.boinc_page_provenance DROP CONSTRAINT IF EXISTS boinc_page_provenance_status_check;
ALTER TABLE quality.boinc_page_provenance DROP CONSTRAINT IF EXISTS status_known;
-- `no_wu` was renamed: a page with no workunit row left has no host to hold, and is accepted
-- unless its engine image is held.
UPDATE quality.boinc_page_provenance SET status = 'host_unknown' WHERE status = 'no_wu';
ALTER TABLE quality.boinc_page_provenance ADD CONSTRAINT status_known CHECK (status IN
  ('resolved', 'host_unknown', 'no_canonical', 'result_missing', 'ambiguous_prefix', 'ambiguous_duplicate_wu'));
ALTER TABLE quality.boinc_page_provenance DROP CONSTRAINT IF EXISTS boinc_page_provenance_check;
ALTER TABLE quality.boinc_page_provenance DROP CONSTRAINT IF EXISTS resolved_has_host;
ALTER TABLE quality.boinc_page_provenance ADD CONSTRAINT resolved_has_host
  CHECK (status <> 'resolved' OR (hostid IS NOT NULL AND wu_id IS NOT NULL));
CREATE INDEX IF NOT EXISTS boinc_page_provenance_host
  ON quality.boinc_page_provenance (hostid, image12) WHERE status = 'resolved';

GRANT SELECT ON quality.boinc_page_provenance TO quality_loader, quality_text_writer, quality_reader;
GRANT INSERT, UPDATE ON quality.boinc_page_provenance TO quality_loader;
