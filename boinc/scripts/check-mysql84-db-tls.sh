#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ne 0 ]; then
  echo "usage: boinc/scripts/check-mysql84-db-tls.sh (private MySQL 8.4 fixture; no Azure)" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
docker info >/dev/null
mysql_image=mirror.gcr.io/library/mysql:8.4@sha256:24989179f58856db763f418010c1169fecc944e3ec1ea7e921572d83eb070f70
name="usnm-mysql84-tls-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
tag="usnm-boinc-db-tls:$(git rev-parse --short HEAD)"
cleanup() {
  docker rm -f "$name-client" "$name-db" >/dev/null 2>&1 || true
  docker volume rm "$name-data" "$name-certs" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker build --memory 1536m -f boinc/scripts/db-tls/Containerfile -t "$tag" .
docker volume create "$name-data" >/dev/null
docker volume create "$name-certs" >/dev/null
docker run --rm --name "$name-client" --network none --read-only --cpus 1 --memory 256m --pids-limit 32 \
  --cap-drop ALL --security-opt no-new-privileges \
  --mount "type=volume,src=$name-certs,dst=/fixture" \
  --tmpfs /usr/local/share/usnm:rw,nosuid,noexec,size=1m \
  --entrypoint python3 "$tag" /checks/mysql84.py prepare
# Bypass image initialization scripts: no generated admin password, networking or
# root-to-mysql identity transition. All credentials and state are disposable.
docker run --rm --name "$name-db" --network none --read-only --user 0:0 \
  --cpus 1 --memory 768m --pids-limit 128 --cap-drop ALL --security-opt no-new-privileges \
  --mount "type=volume,src=$name-data,dst=/var/lib/mysql" \
  --tmpfs /var/run/mysqld:rw,nosuid,noexec,size=32m \
  --entrypoint mysqld "$mysql_image" --no-defaults --initialize-insecure --user=root \
  --datadir=/var/lib/mysql --tmpdir=/var/run/mysqld --log-error=/var/lib/mysql/init.log
start_db() {
  docker run -d --name "$name-db" --network none --read-only --user 0:0 \
    --cpus 1 --memory 768m --pids-limit 128 --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=volume,src=$name-data,dst=/var/lib/mysql" \
    --mount "type=volume,src=$name-certs,dst=/fixture,readonly" \
    --tmpfs /var/run/mysqld:rw,nosuid,noexec,size=32m \
    --add-host tls-db.test:127.0.0.1 --add-host wrong-db.test:127.0.0.1 \
    --entrypoint mysqld "$mysql_image" --no-defaults --user=root \
    --datadir=/var/lib/mysql --tmpdir=/var/run/mysqld --socket=/var/run/mysqld/mysql.sock \
    --pid-file=/var/run/mysqld/mysql.pid --log-error=/var/lib/mysql/server.log \
    --bind-address=127.0.0.1 --port=3306 --mysqlx=OFF --skip-name-resolve \
    --innodb-buffer-pool-size=64M --performance-schema=OFF --max-connections=16 \
    --require-secure-transport=ON --ssl-ca=/fixture/ca.pem \
    --ssl-cert=/fixture/server.pem --ssl-key=/fixture/server.key >/dev/null
  for _ in $(seq 1 120); do
    if docker exec "$name-db" mysql --no-defaults --socket=/var/run/mysqld/mysql.sock \
      -uroot --batch --skip-column-names -e 'SELECT 1' >/dev/null 2>&1; then return; fi
    if [[ $(docker inspect "$name-db" --format '{{.State.Running}}') != true ]]; then
      echo "private MySQL 8.4 exited before readiness" >&2
      return 1
    fi
    sleep 1
  done
  echo "private MySQL 8.4 readiness timed out" >&2
  return 1
}
start_db
docker exec "$name-db" mysql --no-defaults --socket=/var/run/mysqld/mysql.sock -uroot -e \
  "CREATE DATABASE fixture; CREATE USER 'fixture'@'127.0.0.1' IDENTIFIED BY 'fixture-only'; GRANT ALL ON fixture.* TO 'fixture'@'127.0.0.1';"
for phase in matrix restart; do
  if [[ $phase == restart ]]; then
    docker stop -t 30 "$name-db" >/dev/null
    docker rm "$name-db" >/dev/null
    start_db
  fi
  docker run --rm --name "$name-client" --network "container:$name-db" --read-only \
    --cpus 1 --memory 256m --pids-limit 32 --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=volume,src=$name-certs,dst=/fixture,readonly" \
    --tmpfs /usr/local/share/usnm:rw,nosuid,noexec,size=1m \
    --entrypoint python3 "$tag" /checks/mysql84.py "$phase"
done
