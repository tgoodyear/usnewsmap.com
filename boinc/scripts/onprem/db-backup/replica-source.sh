#!/usr/bin/env bash
# The BOINC host's (CT 230's) side of the live MySQL replica on pve-big (home-infra
# scripts/lab/boinc-db-replica-ct.sh). Run as root on CT 230. Online only: it changes no MySQL
# setting that needs a restart, and restarts nothing.
#
#   replica-source.sh --tunnel --pubkey FILE
#   replica-source.sh --users --password-stdin
#   replica-source.sh --monitor-user --password-stdin
#   replica-source.sh --clone-donor on --password-stdin
#   replica-source.sh --clone-donor off
#   replica-source.sh --sign-csr FILE --out FILE
#
# MySQL here listens on 127.0.0.1 only (mysql-setup.sh), so the replica reaches it through ssh:
# --tunnel makes the system user usnm-db-tunnel, whose one key (--pubkey, the replica's) may
# forward to 127.0.0.1:3306 here, and listen on 127.0.0.1:3306 here for the failover
# (db-replica.md), which only works once this host's MySQL is stopped: no shell, no other
# forwards. It prints this host's ed25519 host key for the replica's known_hosts.
# --users makes usnmrepl@127.0.0.1 (REPLICATION SLAVE, TLS only) with the password on stdin.
# --monitor-user makes usnmmonitor@127.0.0.1 (REPLICATION CLIENT only, TLS only), which the feed
# monitor's dbbackup source uses on the replica for SHOW REPLICA STATUS; it reaches the replica
# by replication.
# --clone-donor on installs the clone plugin (it stays installed) and makes usnmclone@127.0.0.1
# (BACKUP_ADMIN, TLS only) with the password on stdin, for the replica's one-time
# CLONE INSTANCE; --clone-donor off drops that user again.
# --sign-csr signs the replica's server certificate with this database's CA, so the replica
# presents the same CA and names (usnm-boinc-mysql, localhost, 127.0.0.1) as this server, and the
# BOINC daemons trust it unchanged after a failover (docs/boinc/server/db-replica.md).
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
die() { echo "error: $*" >&2; exit 1; }
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
DO="" PUBKEY="" PW_STDIN=false CLONE="" CSR="" OUT=""
while [ $# -gt 0 ]; do
  case $1 in
    --tunnel) DO=tunnel; shift ;;
    --pubkey) PUBKEY=${2:?}; shift 2 ;;
    --users) DO=users; shift ;;
    --monitor-user) DO=monitor; shift ;;
    --clone-donor) DO=clone; CLONE=${2:?}; shift 2 ;;
    --password-stdin) PW_STDIN=true; shift ;;
    --sign-csr) DO=sign; CSR=${2:?}; shift 2 ;;
    --out) OUT=${2:?}; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$DO" ] || usage
[ "$(id -u)" = 0 ] || die "run as root"
SEC=/etc/usnm-boinc/secrets NAME=usnm-boinc-mysql TLS=/srv/usnm-boinc/mysql-tls

rootsql() {
  local cnf; cnf=$(mktemp /dev/shm/my.XXXXXX)
  printf '[client]\nuser=root\npassword=%s\nhost=%s\nssl-mode=VERIFY_IDENTITY\nssl-ca=/tls/ca.pem\n' "$(cat "$SEC/db-root")" "$NAME" > "$cnf"
  podman run -i --rm --log-driver none --network host --add-host "$NAME:127.0.0.1" -v "$cnf:/run/my.cnf:ro" -v "$TLS/ca.pem:/tls/ca.pem:ro" \
    "$(podman inspect "$NAME" --format '{{.Image}}')" mysql --defaults-extra-file=/run/my.cnf -N -B || { rm -f "$cnf"; return 1; }
  rm -f "$cnf"
}
password() {
  $PW_STDIN || die "--password-stdin"
  local pw; IFS= read -r pw || true
  [[ $pw =~ ^[A-Za-z0-9]{24,}$ ]] || die "stdin: a password of 24+ letters and digits"
  printf '%s' "$pw"
}

case $DO in
  tunnel)
    [ -r "$PUBKEY" ] || die "--pubkey FILE"
    key=$(head -1 "$PUBKEY")
    [[ $key =~ ^ssh-ed25519\ [A-Za-z0-9+/=]+(\ .*)?$ ]] || die "--pubkey: one ssh-ed25519 public key"
    id usnm-db-tunnel > /dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/usnm-db-tunnel --shell /usr/sbin/nologin usnm-db-tunnel
    install -d -m 700 -o usnm-db-tunnel -g usnm-db-tunnel /var/lib/usnm-db-tunnel/.ssh
    printf 'restrict,port-forwarding,permitopen="127.0.0.1:3306",permitlisten="127.0.0.1:3306",command="/bin/false" %s\n' "$key" > /var/lib/usnm-db-tunnel/.ssh/authorized_keys
    chown usnm-db-tunnel:usnm-db-tunnel /var/lib/usnm-db-tunnel/.ssh/authorized_keys
    chmod 600 /var/lib/usnm-db-tunnel/.ssh/authorized_keys
    cut -d' ' -f1,2 /etc/ssh/ssh_host_ed25519_key.pub
    ;;
  users)
    pw=$(password)
    rootsql <<SQL
CREATE USER IF NOT EXISTS 'usnmrepl'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
ALTER USER 'usnmrepl'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
GRANT REPLICATION SLAVE ON *.* TO 'usnmrepl'@'127.0.0.1';
SQL
    echo "usnmrepl ready"
    ;;
  monitor)
    pw=$(password)
    rootsql <<SQL
CREATE USER IF NOT EXISTS 'usnmmonitor'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
ALTER USER 'usnmmonitor'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
GRANT REPLICATION CLIENT ON *.* TO 'usnmmonitor'@'127.0.0.1';
SQL
    echo "usnmmonitor ready"
    ;;
  clone)
    case $CLONE in
      on)
        pw=$(password)
        [ "$(echo "SELECT COUNT(*) FROM information_schema.plugins WHERE plugin_name='clone'" | rootsql)" = 1 ] ||
          echo "INSTALL PLUGIN clone SONAME 'mysql_clone.so';" | rootsql
        rootsql <<SQL
CREATE USER IF NOT EXISTS 'usnmclone'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
ALTER USER 'usnmclone'@'127.0.0.1' IDENTIFIED BY '$pw' REQUIRE SSL;
GRANT BACKUP_ADMIN ON *.* TO 'usnmclone'@'127.0.0.1';
SQL
        echo "clone donor ready" ;;
      off) echo "DROP USER IF EXISTS 'usnmclone'@'127.0.0.1';" | rootsql; echo "usnmclone dropped" ;;
      *) die "--clone-donor on|off" ;;
    esac
    ;;
  sign)
    [ -r "$CSR" ] && [ -n "$OUT" ] || die "--sign-csr FILE --out FILE"
    openssl req -in "$CSR" -noout -verify 2> /dev/null || die "$CSR isn't a valid request"
    ext=$(mktemp)
    printf 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' "$NAME" > "$ext"
    openssl x509 -req -in "$CSR" -CA "$TLS/ca.pem" -CAkey "$TLS/ca-key.pem" -CAcreateserial -days 825 -sha256 \
      -extfile "$ext" -out "$OUT" 2> /dev/null
    rm -f "$ext"
    echo "signed $OUT"
    ;;
esac
