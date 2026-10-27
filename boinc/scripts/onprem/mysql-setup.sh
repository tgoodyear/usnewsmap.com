#!/usr/bin/env bash
# One-time setup of the on-prem BOINC MySQL on CT 230 (#942). Safe to re-run: keeps the CA,
# certs, data and secrets that already exist.
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
unset XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS
B=/srv/usnm-boinc; TLS=$B/mysql-tls; DATA=$B/mysql-data; SEC=/etc/usnm-boinc/secrets; NAME=usnm-boinc-mysql
IMG=docker.io/library/mysql:8.4
mkdir -p "$TLS" "$DATA" "$SEC" /etc/usnm-boinc/mysql.conf.d; chmod 700 "$SEC"

# Key Vault through the Arc identity.
EP=http://localhost:40342/metadata/identity/oauth2/token; url="$EP?api-version=2020-06-01&resource=https://vault.azure.net"
realm=$(curl -sS -o /dev/null -D - -H "Metadata: true" "$url" | tr -d '\r' | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *Basic realm=\([^[:space:]]*\).*/\1/p')
case "$realm" in /var/opt/azcmagent/tokens/*.key) [[ $realm != *..* ]] && [ -f "$realm" ] ;; *) echo "no Arc challenge" >&2; exit 1 ;; esac
tok=$(curl -fsS -H "Metadata: true" -H @<(printf 'Authorization: Basic %s\n' "$(cat "$realm")") "$url" | jq -er .access_token)
kv() { curl -fsS -H @<(printf 'Authorization: Bearer %s\n' "$tok") "https://usnmboinckvprodv7n7u6.vault.azure.net/secrets/$1?api-version=7.4" | jq -j .value; }
umask 077
[ -s "$SEC/db-admin" ] || kv mysql-admin-password > "$SEC/db-admin"
[ -s "$SEC/db-runtime" ] || kv mysql-runtime-password > "$SEC/db-runtime"
[ -s "$SEC/db-root" ] || openssl rand -hex 24 | tr -d '\n' > "$SEC/db-root"
unset tok
umask 022

# A private CA for this database only; the BOINC containers trust it in place of Azure's roots.
if [ ! -s "$TLS/ca.pem" ]; then
  (umask 077; openssl genrsa -out "$TLS/ca-key.pem" 3072 2>/dev/null)
  openssl req -x509 -new -key "$TLS/ca-key.pem" -sha256 -days 3650 -subj "/CN=usnm-boinc MySQL CA" -out "$TLS/ca.pem"
fi
if [ ! -s "$TLS/server-cert.pem" ]; then
  (umask 077; openssl genrsa -out "$TLS/server-key.pem" 3072 2>/dev/null)
  openssl req -new -key "$TLS/server-key.pem" -subj "/CN=$NAME" -out "$TLS/server.csr"
  printf 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' "$NAME" > "$TLS/ext.cnf"
  openssl x509 -req -in "$TLS/server.csr" -CA "$TLS/ca.pem" -CAkey "$TLS/ca-key.pem" -CAcreateserial -days 825 -sha256 -extfile "$TLS/ext.cnf" -out "$TLS/server-cert.pem" 2>/dev/null
  rm -f "$TLS/server.csr" "$TLS/ext.cnf"
fi
# mysqld runs as uid 999 in the image.
chown 999:999 "$TLS/server-key.pem" "$DATA"; chmod 600 "$TLS/server-key.pem"
cp "$TLS/ca.pem" /usr/local/share/usnm-mysql-ca.pem 2>/dev/null || { mkdir -p /usr/local/share; cp "$TLS/ca.pem" /usr/local/share/usnm-mysql-ca.pem; }

cat > /etc/usnm-boinc/mysql.conf.d/usnm.cnf <<CNF
[mysqld]
require_secure_transport = ON
ssl_ca   = /tls/ca.pem
ssl_cert = /tls/server-cert.pem
ssl_key  = /tls/server-key.pem
tls_version = TLSv1.2,TLSv1.3
innodb_buffer_pool_size = 1G
max_connections = 200
skip_name_resolve = ON
bind_address = 127.0.0.1
mysqlx = OFF
binlog_expire_logs_seconds = 259200
CNF

# Every BOINC container uses the host network; the CNI backend here has no DNS for names.
grep -q "[[:space:]]$NAME\$" /etc/hosts || echo "127.0.0.1 $NAME" >> /etc/hosts
podman pull -q "$IMG" >/dev/null
DIGEST=$(podman image inspect "$IMG" --format '{{.Digest}}')
cat > /etc/systemd/system/$NAME.service <<UNIT
[Unit]
Description=usnm BOINC MySQL (#942)
Wants=network-online.target
After=network-online.target

[Service]
Restart=on-failure
TimeoutStopSec=120
ExecStartPre=-/usr/bin/podman rm -f $NAME
ExecStart=/usr/bin/podman run --rm --name $NAME --network host \\
  -v $DATA:/var/lib/mysql:Z -v $TLS:/tls:ro -v /etc/usnm-boinc/mysql.conf.d:/etc/mysql/conf.d:ro \\
  -v $SEC/db-root:/run/secrets/db-root:ro -e MYSQL_ROOT_PASSWORD_FILE=/run/secrets/db-root \\
  --stop-timeout 110 docker.io/library/mysql@$DIGEST
ExecStop=/usr/bin/podman stop -t 110 $NAME

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable -q --now $NAME
for _ in $(seq 1 60); do podman exec $NAME mysqladmin --user=root --password="$(cat $SEC/db-root)" ping >/dev/null 2>&1 && break; sleep 3; done

# Users as the Azure server has them: admin for init/migrations, runtime for the daemons.
cnf=$(mktemp /dev/shm/my.XXXXXX); trap 'rm -f "$cnf"' EXIT
printf '[client]\nuser=root\npassword=%s\nhost=127.0.0.1\nssl-mode=VERIFY_IDENTITY\nssl-ca=%s\n' "$(cat $SEC/db-root)" "$TLS/ca.pem" > "$cnf"
podman run -i --rm --network host --add-host $NAME:127.0.0.1 -v "$cnf:/run/my.cnf:ro" -v "$TLS/ca.pem:$TLS/ca.pem:ro" "docker.io/library/mysql@$DIGEST" \
  mysql --defaults-extra-file=/run/my.cnf --host=$NAME <<SQL
CREATE DATABASE IF NOT EXISTS usnmocr;
CREATE USER IF NOT EXISTS 'usnmboincadmin'@'%' IDENTIFIED BY '$(cat $SEC/db-admin)';
ALTER USER 'usnmboincadmin'@'%' IDENTIFIED BY '$(cat $SEC/db-admin)';
GRANT ALL PRIVILEGES ON usnmocr.* TO 'usnmboincadmin'@'%';
CREATE USER IF NOT EXISTS 'usnmboincrun'@'%' IDENTIFIED BY '$(cat $SEC/db-runtime)';
ALTER USER 'usnmboincrun'@'%' IDENTIFIED BY '$(cat $SEC/db-runtime)';
GRANT SELECT, INSERT, UPDATE, DELETE ON usnmocr.* TO 'usnmboincrun'@'%';
SELECT CONCAT('MYSQL ', VERSION(), ' ssl=', (SELECT VARIABLE_VALUE FROM performance_schema.session_status WHERE VARIABLE_NAME='Ssl_version')) AS s;
SQL
echo "image digest $DIGEST"
