#!/usr/bin/env bash
# A throwaway PostgreSQL + pgmq for CI (#967), set up like the queue container: TLS 1.3 only,
# network clients by client certificate (CN = role), the usnm_queue database with setup.sql
# applied and test_roles.sql checked. Prints the postgres:// location for --role, for
# usnm-store's live test (USNM_TEST_PGMQ).
#
#   ci-fixture.sh --workdir <dir> [--role ingest_producer] [--port 54329] [--name usnm-queue-ci]
#
# Needs docker and openssl. The image is pgmq's own PostgreSQL 17 image at the pgmq version
# the queue container runs (home-infra scripts/lab/queue-ct.sh), pinned by digest.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
IMAGE=ghcr.io/pgmq/pg17-pgmq:v1.13.0@sha256:1ce7c40f588a08cfad14bc2b228eb35db98270163e27308cec109762b8120c85
WORK="" ROLE=ingest_producer PORT=54329 NAME=usnm-queue-ci
while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORK=${2:-}; shift 2 ;;
    --role) ROLE=${2:-}; shift 2 ;;
    --port) PORT=${2:-}; shift 2 ;;
    --name) NAME=${2:-}; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$WORK" ] || die "--workdir is required"
[[ $ROLE =~ ^[a-z][a-z0-9_]{0,40}$ ]] || die "--role: a role name"
[[ $PORT =~ ^[0-9]+$ ]] || die "--port: a number"
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$WORK/certs" "$WORK/client"
C=$WORK/certs

openssl genrsa -out "$C/ca.key" 2048 2> /dev/null
openssl req -x509 -new -key "$C/ca.key" -sha256 -days 2 -subj "/CN=usnm-queue CI CA" -out "$C/ca.pem"
openssl genrsa -out "$C/server.key" 2048 2> /dev/null
openssl req -new -key "$C/server.key" -subj "/CN=localhost" -out "$C/server.csr"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > "$C/server.ext"
openssl x509 -req -in "$C/server.csr" -CA "$C/ca.pem" -CAkey "$C/ca.key" -CAcreateserial -days 2 \
  -sha256 -extfile "$C/server.ext" -out "$C/server.crt" 2> /dev/null
openssl genrsa -out "$WORK/client/client.key" 2048 2> /dev/null
openssl req -new -key "$WORK/client/client.key" -subj "/CN=$ROLE" -out "$C/client.csr"
printf 'extendedKeyUsage=clientAuth\n' > "$C/client.ext"
openssl x509 -req -in "$C/client.csr" -CA "$C/ca.pem" -CAkey "$C/ca.key" -CAcreateserial -days 2 \
  -sha256 -extfile "$C/client.ext" -out "$WORK/client/client.crt" 2> /dev/null
cp "$C/ca.pem" "$WORK/client/ca.pem"
chmod 600 "$WORK/client/client.key"
# The same rules as the queue container: local admin, and certificates over TLS from the network.
cat > "$C/pg_hba.conf" <<'HBA'
local   all          all                 trust
hostssl usnm_queue   all       all       cert
HBA
# The image runs as postgres, a different uid from this user: the mounted copy of this
# throwaway key must be readable to it (the server's own copy is 600, below).
chmod 644 "$C/server.key"
docker rm -f "$NAME" > /dev/null 2>&1 || true
# The server must own its key (mode 600): the container copies the mounted files to its own
# directory and sets that before the image's entrypoint starts PostgreSQL. A bind mount's owner
# can't be set portably from the host.
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:5432" -e POSTGRES_HOST_AUTH_METHOD=trust \
  -v "$C:/certs-in:ro" --entrypoint sh "$IMAGE" -c '
    set -e; mkdir -p /tmp/certs; chmod 700 /tmp/certs
    cp /certs-in/server.crt /certs-in/server.key /certs-in/ca.pem /certs-in/pg_hba.conf /tmp/certs/
    [ "$(id -u)" != 0 ] || chown -R postgres:postgres /tmp/certs
    chmod 600 /tmp/certs/server.key
    exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/certs/server.crt \
      -c ssl_key_file=/tmp/certs/server.key -c ssl_ca_file=/tmp/certs/ca.pem \
      -c ssl_min_protocol_version=TLSv1.3 -c hba_file=/tmp/certs/pg_hba.conf' > /dev/null
for _ in $(seq 60); do
  docker exec "$NAME" pg_isready -U postgres -q 2> /dev/null && break
  sleep 1
done
docker exec "$NAME" pg_isready -U postgres -q || { docker logs "$NAME" >&2; die "PostgreSQL didn't start"; }
psql_admin() { docker exec -i "$NAME" psql -q -v ON_ERROR_STOP=1 -U postgres "$@"; }
psql_admin -c 'CREATE DATABASE usnm_queue'
psql_admin -d usnm_queue -c 'CREATE EXTENSION pgmq'
psql_admin -d usnm_queue < "$here/setup.sql" > /dev/null
psql_admin -d usnm_queue < "$here/setup.sql" > /dev/null # safe to run again
psql_admin -d usnm_queue < "$here/test_roles.sql" >&2
printf 'postgres://localhost:%s/usnm_queue?user=%s&sslcert=%s&sslkey=%s&sslrootcert=%s\n' \
  "$PORT" "$ROLE" "$WORK/client/client.crt" "$WORK/client/client.key" "$WORK/client/ca.pem"
