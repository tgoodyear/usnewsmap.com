-- Which BOINC host read a published page (#1956). One row per `route=boinc` page of
-- `curated/ocr-local/pages`, built from the BOINC database while the workunit and result rows
-- still exist (db_purge removes them 30 days after a workunit is done). Run as postgres in the
-- queue container; safe to run again:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f provenance.sql
--
-- `status` says how far the page resolved; only `resolved` rows carry a host. A later build
-- never replaces a `resolved` row with a less complete one.

CREATE TABLE IF NOT EXISTS quality.boinc_page_provenance (
  doc_id              text PRIMARY KEY,
  job_id              text,
  wu_id               integer,
  canonical_result_id integer,
  hostid              integer,
  app_version_id      integer,
  platform            text,
  -- 'arm64' or 'amd64' from the host row (os_name, p_vendor), never from the platform.
  family              text CHECK (family IN ('arm64', 'amd64')),
  -- 'wu_name' (usnm_<job id>), 'bundle_member' (usnm_ocrb_<bundle>, by its member list), either
  -- with '+dup_wu_same_host' when the job also ran in a second workunit on the same host.
  resolved_via        text,
  resolved_at         timestamptz NOT NULL,
  status              text NOT NULL CHECK (status IN
    ('resolved', 'no_wu', 'no_canonical', 'result_missing', 'ambiguous_prefix', 'ambiguous_duplicate_wu')),
  -- The 12 hex of the engine image the published row names, and the full digest where a bundle
  -- request still lists the job.
  image12             text NOT NULL,
  image_digest        text,
  -- Hosts behind a job that ran in two workunits with different canonical hosts.
  candidate_host_ids  integer[],
  CHECK (status <> 'resolved' OR (hostid IS NOT NULL AND wu_id IS NOT NULL AND family IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS boinc_page_provenance_host
  ON quality.boinc_page_provenance (hostid, image12) WHERE status = 'resolved';

GRANT SELECT ON quality.boinc_page_provenance TO quality_loader, quality_text_writer, quality_reader;
GRANT INSERT, UPDATE ON quality.boinc_page_provenance TO quality_loader;
