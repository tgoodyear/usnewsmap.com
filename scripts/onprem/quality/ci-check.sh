#!/usr/bin/env bash
# CI for the OCR-quality results database (#962): a throwaway PostgreSQL 17 (the queue
# fixture's pinned image), usnm_quality with setup.sql applied twice, the role checks in
# test_roles.sql, load.py's upserts run as quality_loader and quality_text_writer (LoC's
# software among them), the summary's refresh, and rank.sql and the campaign builder's
# export as quality_reader; the provenance and holds tables (#1956).
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
# The BOINC provenance and holds tables (#1956), each applied twice; the provenance constraint
# of the first version (a family on every resolved row) is put back first, to see it replaced.
psql_admin -d usnm_quality < "$here/provenance.sql" > /dev/null
psql_admin -d usnm_quality -c "ALTER TABLE quality.boinc_page_provenance DROP CONSTRAINT resolved_has_host" \
  -c "ALTER TABLE quality.boinc_page_provenance ADD CONSTRAINT boinc_page_provenance_check CHECK (status <> 'resolved' OR (hostid IS NOT NULL AND wu_id IS NOT NULL AND family IS NOT NULL))"
psql_admin -d usnm_quality < "$here/provenance.sql" > /dev/null
psql_admin -d usnm_quality < "$here/holds.sql" > /dev/null
psql_admin -d usnm_quality < "$here/holds.sql" > /dev/null
psql_admin -d usnm_quality < "$here/test_roles.sql" >&2
# provenance_load.sql as the loader: a host with no known CPU family stays resolved, a rerun
# changes nothing, and a later less complete row never degrades a resolved one.
prov() { printf 'sn1_1900-01-01_ed-1_seq-1\tboinc-aaaaaaaaaaaa\tj\t7\t9\t12\t3\tx86_64-pc-linux-gnu\t\\N\twu_name\t2026-11-14T00:00:00Z\t%s\tabcdefabcdef\t\\N\t\\N\n' "$1"; }
prov result_missing | psql_admin -d usnm_quality -f "$here/provenance_load.sql" > /dev/null
prov resolved | psql_admin -d usnm_quality -f "$here/provenance_load.sql" > /dev/null
prov resolved | psql_admin -d usnm_quality -f "$here/provenance_load.sql" > /dev/null
got=$(psql_admin -d usnm_quality -tA -c "SELECT status, hostid, family IS NULL FROM quality.boinc_page_provenance")
[ "$got" = "resolved|12|t" ] || die "a host without a family: $got"
prov result_missing | psql_admin -d usnm_quality -f "$here/provenance_load.sql" > /dev/null
got=$(psql_admin -d usnm_quality -tA -c "SELECT status FROM quality.boinc_page_provenance")
[ "$got" = "resolved" ] || die "a resolved row was degraded: $got"
# Holds: one row added twice is one hold, one naming neither a node nor an engine is refused,
# and the reader sees the row but cannot change it.
hold="INSERT INTO quality.boinc_page_holds (doc_id, engine, reason, created_by) VALUES ('sn1_1900-01-01_ed-1_seq-1', 'tesseract/5.5.0 profile=eng image=sha256:9c8eac0485d5', 'ci', 'ci') ON CONFLICT DO NOTHING"
psql_admin -d usnm_quality -c "$hold" -c "$hold" > /dev/null
psql_admin -d usnm_quality -c "INSERT INTO quality.boinc_page_holds (doc_id, reason, created_by) VALUES ('x', 'r', 'c')" > /dev/null 2>&1 \
  && die "a hold naming neither node nor engine was accepted"
got=$(psql_admin -d usnm_quality -tA -c "SET ROLE quality_reader" -c "SELECT count(*) FROM quality.boinc_page_holds" | tail -1)
[ "$got" = "1" ] || die "holds as the reader: $got"
psql_admin -d usnm_quality -c "SET ROLE quality_reader" -c "DELETE FROM quality.boinc_page_holds" > /dev/null 2>&1 \
  && die "the reader deleted a hold"
psql_admin -d usnm_quality -c "DELETE FROM quality.boinc_page_holds" -c "DELETE FROM quality.boinc_page_provenance" > /dev/null
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
                                "job_id": "j", "blocks": 3, "pairs_json": '{"as": {"cer_sym": 0.4}}',
                                "qe_version": "qe-v1", "qe_resources": "a" * 64, "qe_words": 10, "qe_known3": 7,
                                "qe_no_words": False, "qe_oov": 0.1, "qe_charlm_bpc": 2.0, "qe_lm_oov": 2.542466}], "v1-ci")
print(m.campaign_sql("damage-v2", "damage") + m.campaign_sql("v1-ci", "boinc-quality", engine="sha256:" + "a" * 64)
      + m.upsert_sql("pages", m.PAGE_COLS, ["doc_id"], pages) + m.upsert_sql("damage", m.DAMAGE_COLS, ["doc_id"], damage)
      + m.upsert_sql("unit_pages", m.UNIT_COLS, ["campaign", "doc_id"], units)
      + m.upsert_sql("reader_scores", m.SCORE_COLS, ["campaign", "doc_id", "reader"], scores)
      + m.upsert_sql("pages", m.PAGE_COLS, ["doc_id"], pages))  # a rerun replaces
sw = {"batch": "b", "batch_version": 1, "archive": "b_ver01.tar.bz2", "header_doc": d, "engine_family": "abbyy",
      "software": '[{"block":1,"step":"pre","name":"CCS docWorks","version":"7.0-1.52"},{"block":1,"step":"ocr","name":"FineReader","version":"11.0"}]',
      "agencies": "[]", "steps": '["Align"]', "settings_engines": "[]", "engine_name": "FineReader", "engine_version": "11.0",
      "wrapper": "CCS docWorks", "wrapper_version": "7.0-1.52", "vendor": "CCS Content Conversion Specialists",
      "read_at": "2026-11-03T21:45:00Z"}
rows = m.locsw_rows([sw, {**sw, "engine_version": "12.0"}])  # one row a batch version, the last kept
print(m.upsert_sql("loc_software", m.LOCSW_COLS, ["batch", "batch_version"], rows) * 2)
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
  # A campaign excluded before it loads: its estimate for the page must not count, even
  # though its unit is the newer one.
  echo "RESET ROLE;"
  echo "INSERT INTO quality.excluded_estimates (campaign, why) VALUES ('ci-excluded', 'ci') ON CONFLICT DO NOTHING;"
  echo "SET ROLE quality_loader;"
  python3 - "$here/load.py" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("load", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
units, scores = m.result_rows([{"doc_id": "sn1_1900-01-01_ed-1_seq-1", "reader": "loc-hist", "est_cer": "0.9", "outcome": "ok",
                                "job_id": "k", "blocks": 3, "pairs_json": None}], "ci-excluded")
print(m.campaign_sql("ci-excluded", "boinc-quality") + m.upsert_sql("unit_pages", m.UNIT_COLS, ["campaign", "doc_id"], units)
      + m.upsert_sql("reader_scores", m.SCORE_COLS, ["campaign", "doc_id", "reader"], scores))
PY
  echo "SELECT quality.refresh_summary();"
} | psql_admin -d usnm_quality > /dev/null
got=$(psql_admin -d usnm_quality -tA -c "SELECT doc_id, damage_rate, loc_est_cer, has_image_features, loc_as_local_cer_sym FROM quality.page_summary")
[ "$got" = "sn1_1900-01-01_ed-1_seq-1|0.4|0.3|t|0.08" ] || die "page_summary after the load: $got"
got=$(psql_admin -d usnm_quality -tA -c "SELECT qe_version, qe_known3, qe_no_words, round(qe_lm_oov::numeric, 3) FROM quality.reader_scores WHERE campaign = 'v1-ci'")
[ "$got" = "qe-v1|7|f|2.542" ] || die "reader_scores' quality measure after the load: $got"
got=$(psql_admin -d usnm_quality -tA -c "SELECT engine_family, engine_version, software->1->>'name', vendor FROM quality.pages p JOIN quality.loc_software USING (batch, batch_version)")
[ "$got" = "abbyy|12.0|FineReader|CCS Content Conversion Specialists" ] || die "loc_software after the load: $got"
{ echo "SET ROLE quality_reader;"; cat "$here/rank.sql"; } | psql_admin -d usnm_quality -v limit=5 > /dev/null
# Pages at the campaign builder's band edges, rate:occurrences, so that its own test of a row
# is checked against the report's SQL.
export QC_EDGES="0.1:60 0.25:60 0.25000001:60 0.2500001:60 0.0999999:60 0.5:49 0.5:50"
{
  echo "SET ROLE quality_loader;"
  python3 - "$here/load.py" <<'PY'
import datetime, importlib.util, os, sys
spec = importlib.util.spec_from_file_location("load", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
xs = [{"doc_id": f"sn9_1900-01-01_ed-1_seq-{i}", "batch": "b", "batch_version": 1, "lccn": "sn9",
       "date": datetime.date(1900, 1, 1), "edition": 1, "seq": i, "lang": "eng", "damage_rate": rate,
       "decision": "detected", "head": occurrences, "near": 0}
      for i, (rate, occurrences) in enumerate((e.split(":") for e in os.environ["QC_EDGES"].split()), 1)]
pages, damage = m.damage_rows(xs, "damage-v2")
print(m.upsert_sql("pages", m.PAGE_COLS, ["doc_id"], pages) + m.upsert_sql("damage", m.DAMAGE_COLS, ["doc_id"], damage))
PY
  echo "SELECT quality.refresh_summary();"
} | psql_admin -d usnm_quality > /dev/null
# The builder's export runs as the reader, so its columns and grants are checked, and the bands
# it gives the rows are the report's.
qc="$here/../../../boinc/scripts/onprem/quality-feed/quality_campaign.py"
export_csv=$({
  echo "SET ROLE quality_reader;"
  python3 - "$qc" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("quality_campaign", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(m.export_sql(m.DEFAULT_FILTERS))
PY
} | psql_admin -d usnm_quality)
worst=$(psql_admin -d usnm_quality -tA -c "SET ROLE quality_reader" \
  -c "SELECT doc_id FROM quality.page_summary WHERE damage_rate > 0.25")
QC_EXPORT=$export_csv QC_WORST=$worst python3 - "$qc" <<'PY' || die "the campaign export"
import csv, importlib.util, io, os, sys
spec = importlib.util.spec_from_file_location("quality_campaign", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
rows = list(csv.reader(io.StringIO(os.environ["QC_EXPORT"])))
if rows[:1] != [m.COLUMNS]:
    sys.exit(f"columns {rows[:1]}")
want = {}
for i, edge in enumerate(os.environ["QC_EDGES"].split(), 1):
    rate, occurrences = edge.split(":")
    band = m.band_of(float(rate), int(occurrences), m.DEFAULT_FILTERS)
    if band:
        want[f"sn9_1900-01-01_ed-1_seq-{i}"] = band
worst = set(os.environ["QC_WORST"].split())
got = {r[0]: m.band_of(float(r[8]), int(r[9]), m.DEFAULT_FILTERS) for r in rows[1:]}
sql = {r[0]: "worst" if r[0] in worst else "moderate" for r in rows[1:]}
if not got == sql == want:
    sys.exit(f"bands: the builder's on the export {got}, the report's SQL {sql},"
             f" the builder's on the rates loaded {want}")
PY
# The gain measure's export runs as the reader too: its columns and grants (#1415).
qg="$here/../../../boinc/scripts/onprem/quality-feed/quality_gain.py"
gain_csv=$({
  echo "SET ROLE quality_reader;"
  python3 - "$qg" <<'PY'
import importlib.util, os, sys
sys.path[:0] = [os.path.dirname(sys.argv[1]), os.path.join(os.path.dirname(sys.argv[1]), "..", "ocr-feed")]
spec = importlib.util.spec_from_file_location("quality_gain", sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(m.export_sql(["ours-tesseract"]))
PY
} | psql_admin -d usnm_quality)
[ "$(printf '%s\n' "$gain_csv" | head -1)" = "doc_id,reader,lang,decade,occurrences,loc_damage,ours_damage,loc_est_cer,ours_est_cer" ] \
  || die "the gain export's columns"
echo "usnm_quality checked: schema, roles, loader upserts, LoC software, page_summary, rank queries, campaign and gain exports"
