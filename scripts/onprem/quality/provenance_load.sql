-- Loads a provenance TSV (boinc_provenance.py --out) into quality.boinc_page_provenance. Safe to
-- run again: a resolved row, or one that records the candidate hosts of an ambiguous job, is
-- never replaced by a less complete one. Run as postgres or quality_loader, with the TSV on stdin:
--
--   psql -v ON_ERROR_STOP=1 -d usnm_quality -f provenance_load.sql < provenance.tsv
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE prov_in (LIKE quality.boinc_page_provenance) ON COMMIT DROP;
\copy prov_in (doc_id, node, job_id, wu_id, canonical_result_id, hostid, app_version_id, platform, family, resolved_via, resolved_at, status, image12, image_digest, candidate_host_ids) FROM pstdin
INSERT INTO quality.boinc_page_provenance SELECT * FROM prov_in
ON CONFLICT (doc_id, node) DO UPDATE SET
  job_id = EXCLUDED.job_id, wu_id = EXCLUDED.wu_id,
  canonical_result_id = EXCLUDED.canonical_result_id, hostid = EXCLUDED.hostid,
  app_version_id = EXCLUDED.app_version_id, platform = EXCLUDED.platform,
  family = EXCLUDED.family, resolved_via = EXCLUDED.resolved_via,
  resolved_at = EXCLUDED.resolved_at, status = EXCLUDED.status, image12 = EXCLUDED.image12,
  image_digest = EXCLUDED.image_digest, candidate_host_ids = EXCLUDED.candidate_host_ids
WHERE EXCLUDED.status = 'resolved'
  OR (quality.boinc_page_provenance.status <> 'resolved'
      AND quality.boinc_page_provenance.candidate_host_ids IS NULL);
COMMIT;
SELECT status, count(*) FROM quality.boinc_page_provenance GROUP BY 1 ORDER BY 2 DESC;
