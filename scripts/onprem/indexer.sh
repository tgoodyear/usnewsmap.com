#!/usr/bin/env bash
# A remote Quickwit indexer for an on-prem release through Kafka (#936):
# run on another host's release container (the run directory shared over
# NFS at the same path), as its usnm user, while the release runs with
# USNM_WRITER_ADVERTISE, USNM_WRITER_PEERS (this host) and
# USNM_WRITER_CLUSTER_DONE=<run dir>/cluster-done.
#
#   scripts/onprem/indexer.sh --run-dir DIR --writer HOST:PORT
#     [--port 7380] [--pipelines 4] [--data /var/lib/usnm-indexer]
#
# Joins the writer's cluster (usnm-writer) with the indexer service only:
# the writer's node keeps the metastore, and the control plane gives this
# node --pipelines of the Kafka source's pipelines. Splits go to the shared
# index root, DIR/qw; the node's own working data stays on the local --data
# directory, not the share. When the release writes DIR/cluster-done (every
# page is in the index), the node stops and the script exits 0, so the
# release's final merges run on the writer alone.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,18s/^# \{0,1\}//p' "$0" >&2; exit 2; }
RUN="" WRITER="" PORT=7380 PIPELINES=4 DATA=/var/lib/usnm-indexer
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) RUN=${2:-}; shift 2 ;;
    --writer) WRITER=${2:-}; shift 2 ;;
    --port) PORT=${2:-}; shift 2 ;;
    --pipelines) PIPELINES=${2:-}; shift 2 ;;
    --data) DATA=${2:-}; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $RUN == /* && -d $RUN/qw ]] || die "--run-dir: the release's run directory, with its qw index root"
[[ $WRITER =~ ^[A-Za-z0-9.-]+:[0-9]+$ ]] || die "--writer: host:port"
[[ $PORT =~ ^[0-9]+$ && $PIPELINES =~ ^[0-9]+$ ]] || die "--port and --pipelines: numbers"
QW=${USNM_QUICKWIT_BIN:-/srv/usnm/bin/quickwit}
[ -x "$QW" ] || die "no quickwit at $QW"
host=$(hostname -f)
ip=$(getent ahostsv4 "$host" | awk 'NR == 1 { print $1 }')
[ -n "$ip" ] || die "$host has no IPv4 address"
rm -rf "$DATA" && mkdir -p "$DATA"
cat > "$DATA/node.yaml" << YAML
version: 0.8
cluster_id: usnm-writer
node_id: indexer-$(hostname -s)
listen_address: 0.0.0.0
advertise_address: $ip
rest:
  listen_port: $PORT
grpc_listen_port: $((PORT + 1))
peer_seeds:
  - $WRITER
data_dir: $DATA/qwdata
metastore_uri: file://$RUN/qw
default_index_root_uri: file://$RUN/qw
indexer:
  split_store_max_num_bytes: 4GiB
  cpu_capacity: $((4 * PIPELINES))
  merge_concurrency: 1
YAML
QW_DISABLE_TELEMETRY=1 "$QW" run --config "$DATA/node.yaml" --service indexer > "$DATA/quickwit.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2> /dev/null || true' EXIT
echo "indexer node $(hostname -s) started (pid $pid), joining $WRITER"
until [ -e "$RUN/cluster-done" ]; do
  kill -0 "$pid" 2> /dev/null || { tail -20 "$DATA/quickwit.log" >&2; die "the indexer node exited before the release was done"; }
  sleep 5
done
echo "the release is done ($(cat "$RUN/cluster-done")); stopping the indexer node"
kill -TERM "$pid"
for _ in $(seq 60); do kill -0 "$pid" 2> /dev/null || break; sleep 1; done
kill -0 "$pid" 2> /dev/null && kill -KILL "$pid"
trap - EXIT
echo "stopped"
