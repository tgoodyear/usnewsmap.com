#!/bin/bash
# CT 230 side of the qe-loc-v1 pipeline (qe_loc_pipeline.sh calls it over `pct exec`). Installed as
# /usr/local/sbin/qe-loc-server. One command a call:
#   --command status               in-flight results, units waiting to be submitted, free GB,
#                                  and units whose files were pruned but are needed again
#   --command submit --count N     qe-text-submit of at most N new units (under the step lock)
#   --command collect              BOINC DB export of the canonical results' hosts, then
#                                  qe-text-collect into quality.qe_loc_scores
#   --command prune --days N       delete the files of units collected more than N days ago
# The writer's DSN file is /project/qetext/writer/dsn (see qe_loc_writer_setup.sh); this script
# never prints it.
set -u
cmd=; count=0; days=14
while [ $# -gt 0 ]; do case "$1" in
  --command) cmd=$2; shift 2;; --count) count=$2; shift 2;; --days) days=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
P=/srv/usnm-boinc/project; Q=$P/qetext; C=usnm-boinc-server
# Runs one SQL statement against the BOINC database from inside the server container (the
# project's own credentials from config.xml; nothing is printed but the rows).
dbq() {
  podman exec -i -e SQL="$1" $C bash -c '
cd /project/usnmocr || exit 1
g() { sed -n "s|.*<$1>\(.*\)</$1>.*|\1|p" config.xml | head -1; }
export MYSQL_PWD="$(g db_passwd)"
mysql --ssl -h "$(g db_host)" -u "$(g db_user)" "$(g db_name)" -N -B -e "$SQL"'
}
case "$cmd" in
status)
  inflight=$(dbq "SELECT COUNT(*) FROM result r JOIN workunit w ON w.id = r.workunitid WHERE w.name LIKE 'usnm_quality_qetext%' AND r.server_state IN (2, 4)")
  staged=0; need=""
  for d in "$Q"/units/*/; do
    [ -d "$d" ] || continue
    id=$(basename "$d")
    if [ -s "$d/qe_text.json" ]; then
      [ -e "$d/submitted" ] || staged=$((staged+1))
    elif [ ! -e "$d/collected" ]; then
      need="$need $id"
    fi
  done
  echo "inflight=${inflight:-0}"
  echo "staged=$staged"
  echo "free_gb=$(df -BG --output=avail "$P" | tail -1 | tr -dc 0-9)"
  echo "need=$need"
  ;;
submit)
  flock -w 1800 -E 75 /run/usnm-boinc-step.lock podman exec $C usnm-boinc quality qe-text-submit \
    --root /project/qetext --project /project/usnmocr --max-units "$count"
  ;;
collect)
  dbq "SELECT w.name, r.hostid, CONCAT(av.version_num, ' ', p.name) FROM workunit w JOIN result r ON r.id = w.canonical_resultid JOIN app_version av ON av.id = r.app_version_id JOIN platform p ON p.id = av.platformid WHERE w.name LIKE 'usnm_quality_qetext%'" > "$Q/hosts.tsv.new" || exit 1
  mv "$Q/hosts.tsv.new" "$Q/hosts.tsv"
  podman exec -e USNM_QUALITY_QE_WRITER_DSN_FILE=/project/qetext/writer/dsn $C usnm-boinc quality qe-text-collect \
    --root /project/qetext --spool /project/usnmocr/results/0 --hosts /project/qetext/hosts.tsv
  ;;
prune)
  # A collected unit's job and resource file go; its markers stay, and a unit sent again after a
  # failed host is shipped from pve-big once more (status lists it under need).
  n=0
  for m in $(find "$Q"/units -mindepth 2 -maxdepth 2 -name collected -mtime +"$days" 2>/dev/null); do
    d=$(dirname "$m")
    [ -e "$d/qe_text.json" ] && rm -f "$d/qe_text.json" "$d/qe.bin" && n=$((n+1))
  done
  echo "pruned=$n"
  ;;
*) echo "usage: qe-loc-server --command status|submit|collect|prune [--count N] [--days N]" >&2; exit 2;;
esac
