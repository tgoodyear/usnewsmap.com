#!/usr/bin/env bash
# CI for the OCR-quality results database (#962): a throwaway PostgreSQL 17 (the queue
# fixture's pinned image), usnm_quality with setup.sql applied twice, the role checks in
# test_roles.sql, load.py's upserts run as quality_loader and quality_text_writer, the
# summary's refresh, and rank.sql as quality_reader.
#
#   ci-check.sh [--name usnm-quality-ci]
#
# Needs docker and python3.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
IMAGE=ghcr.io/pgmq/pg17-pgmq:v1.13.0@sha256:1ce7c40f588a08cfad14bc2b228eb35db98270163e27308cec109762b8120c85
NAME=usnm-quality-ci
while [ $# -gt 0 ]; do
  case "$1" in
    --name) NAME=${2:-}; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done
here=$(cd "$(dirname "$0")" && pwd)
docker rm -f "$NAME" > /dev/null 2>&1 || true
docker run -d --name "$NAME" -e POSTGRES_HOST_AUTH_METHOD=trust "$IMAGE" > /dev/null
trap 'docker rm -f "$NAME" > /dev/null 2>&1 || true' EXIT
for _ in $(seq 60); do
  docker exec "$NAME" pg_isready -h 127.0.0.1 -U postgres -q 2> /dev/null && break
  sleep 1
done
docker exec "$NAME" pg_isready -h 127.0.0.1 -U postgres -q || { docker logs "$NAME" >&2; die "PostgreSQL didn't start"; }
psql_admin() { docker exec -i "$NAME" psql -q -v ON_ERROR_STOP=1 -U postgres "$@"; }
psql_admin -c 'CREATE DATABASE usnm_quality'
psql_admin -d usnm_quality < "$here/setup.sql" > /dev/null
psql_admin -d usnm_quality < "$here/setup.sql" > /dev/null 2>&1 # safe to run again
psql_admin -d usnm_quality < "$here/test_roles.sql" >&2
# load.py's SQL, as the loader: a campaign, a page, its damage and a unit result.
{
  echo "SET ROLE quality_loader;"
  python3 - "$here/load.py" <<'PY'
import datetime, importlib.util, sys
spec = importlib.util.spec_from_file_location("load", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
d = "sn1_1900-01-01_ed-1_seq-1"
x = {"doc_id": d, "batch": "b", "batch_version": 1, "lccn": "sn1", "date": datetime.date(1900, 1, 1),
     "edition": 1, "seq": 1, "lang": "eng", "damage_rate": 0.4, "decision": "detected"}
pages, damage = m.damage_rows([x], "damage-v2")
units, scores = m.result_rows([{"doc_id": d, "reader": "loc-hist", "est_cer": "0.3", "outcome": "ok",
                                "job_id": "j", "blocks": 3, "pairs_json": '{"as": {"cer_sym": 0.4}}'}], "v1-ci")
print(m.campaign_sql("damage-v2", "damage") + m.campaign_sql("v1-ci", "boinc-quality", engine="sha256:" + "a" * 64)
      + m.upsert_sql("pages", m.PAGE_COLS, ["doc_id"], pages) + m.upsert_sql("damage", m.DAMAGE_COLS, ["doc_id"], damage)
      + m.upsert_sql("unit_pages", m.UNIT_COLS, ["campaign", "doc_id"], units)
      + m.upsert_sql("reader_scores", m.SCORE_COLS, ["campaign", "doc_id", "reader"], scores)
      + m.upsert_sql("pages", m.PAGE_COLS, ["doc_id"], pages))  # a rerun replaces
PY
} | psql_admin -d usnm_quality > /dev/null
# The agreement loader's SQL, as the text writer, then the loader's refresh.
{
  echo "SET ROLE quality_text_writer;"
  python3 - "$here/load.py" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("load", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
rows = m.agreement_rows([{"doc_id": "sn1_1900-01-01_ed-1_seq-1", "batch": "b", "batch_version": 1, "metric_version": 1,
                          "cer_sym": 0.7, "local_cer_sym": 0.08, "matched_loc": 0.9, "dice": 0.8}], "text-agreement-v1")
print(m.upsert_sql("campaigns", ["campaign", "source", "notes"], ["campaign"], [("text-agreement-v1", "text-compare", "ci")], update=False)
      + m.upsert_sql("text_compare", m.AGREE_COLS, ["campaign", "doc_id", "reader_a", "reader_b"], rows))
PY
  # v1-wave6 is excluded from the start: its estimate for the page must not count, even
  # though its unit is the newer one.
  echo "RESET ROLE; SET ROLE quality_loader;"
  python3 - "$here/load.py" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("load", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
units, scores = m.result_rows([{"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "est_cer": "0.9", "outcome": "ok",
                                "job_id": "k", "blocks": 3, "pairs_json": None}], "v1-wave6")
print(m.campaign_sql("v1-wave6", "boinc-quality") + m.upsert_sql("unit_pages", m.UNIT_COLS, ["campaign", "doc_id"], units)
      + m.upsert_sql("reader_scores", m.SCORE_COLS, ["campaign", "doc_id", "reader"], scores))
PY
  echo "SELECT quality.refresh_summary();"
} | psql_admin -d usnm_quality > /dev/null
got=$(psql_admin -d usnm_quality -tA -c "SELECT doc_id, damage_rate, loc_est_cer, has_image_features, loc_as_local_cer_sym FROM quality.page_summary")
[ "$got" = "sn1_1900-01-01_ed-1_seq-1|0.4|0.3|t|0.08" ] || die "page_summary after the load: $got"
{ echo "SET ROLE quality_reader;"; cat "$here/rank.sql"; } | psql_admin -d usnm_quality -v limit=5 > /dev/null
echo "usnm_quality checked: schema, roles, loader upserts, page_summary, rank queries"
