#!/usr/bin/env bash
# Imports a dump from r2:usnm-boinc-project/db/NAME.sql.gz into the on-prem usnmocr (#942).
# --name NAME (required); --replace drops usnmocr first.
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin; unset XDG_RUNTIME_DIR
name="" replace=0
while [ $# -gt 0 ]; do case $1 in --name) name=$2; shift 2;; --replace) replace=1; shift;; *) echo "unknown $1" >&2; exit 2;; esac; done
[ -n "$name" ] || { echo "--name is required" >&2; exit 2; }
sed 's#^exec rclone copy.*#exec rclone "$@"#' /usr/local/sbin/usnm-r2-pull > /root/.r2rc; chmod 700 /root/.r2rc
trap 'rm -f /root/.r2rc "$cnf"' EXIT
D=$(podman inspect usnm-boinc-mysql --format '{{.Image}}')
cnf=$(mktemp /dev/shm/my.XXXXXX)
printf '[client]\nuser=root\npassword=%s\nssl-mode=VERIFY_IDENTITY\nssl-ca=/tls/ca.pem\n' "$(cat /etc/usnm-boinc/secrets/db-root)" > "$cnf"
my() { podman run -i --rm --network host --add-host usnm-boinc-mysql:127.0.0.1 -v "$cnf:/run/my.cnf:ro" -v /srv/usnm-boinc/mysql-tls/ca.pem:/tls/ca.pem:ro "$D" mysql --defaults-extra-file=/run/my.cnf --host=usnm-boinc-mysql "$@"; }
[ $replace = 1 ] && echo "DROP DATABASE IF EXISTS usnmocr; CREATE DATABASE usnmocr;" | my
start=$(date +%s)
/root/.r2rc cat "r2:usnm-boinc-project/db/$name.sql.gz" 2>/dev/null | gunzip | my
echo "imported $name in $(( $(date +%s) - start )) s"
echo "SELECT 'workunit', COUNT(*) FROM usnmocr.workunit UNION ALL SELECT 'result', COUNT(*) FROM usnmocr.result UNION ALL SELECT 'host', COUNT(*) FROM usnmocr.host UNION ALL SELECT 'user', COUNT(*) FROM usnmocr.user UNION ALL SELECT 'app_version', COUNT(*) FROM usnmocr.app_version" | my -N
