#!/usr/bin/env bash
# The BOINC server image (--image, contrib-compute/images/server) has the stock daemons PR #441
# runs, the project tools, the wrapper, Apache with PHP, and can make an upload key pair. It does
# not start the project: that needs MySQL, Key Vault and a share (UNVERIFIED, docs/images.md).
set -euo pipefail
usage() { echo "usage: scripts/ci/check-boinc-server-image.sh --image IMAGE" >&2; exit 2; }
image=""
while [ $# -gt 0 ]; do
  case $1 in
    --image) [ $# -ge 2 ] || usage; image=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$image" ] || usage
docker run --rm --entrypoint bash "$image" -c '
set -e
cd /usr/local/boinc
for f in sched/feeder sched/transitioner sched/file_deleter sched/db_purge sched/sample_trivial_validator \
         sched/batch_collect_assimilator sched/cgi sched/file_upload_handler tools/create_work \
         tools/dir_hier_path tools/sign_executable lib/crypt_prog; do
  [ -x "$f" ] || { echo "missing $f" >&2; exit 1; }
done
for f in tools/make_project tools/xadd tools/update_versions tools/upgrade; do
  [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }
done
[ -x /usr/local/lib/usnm-boinc/docker_wrapper ] || { echo "missing docker_wrapper" >&2; exit 1; }
[ -x /usr/local/bin/bridge-job ] || { echo "missing bridge-job" >&2; exit 1; }
for f in security.txt trust.txt gpc.json assetlinks.json dnt-policy.txt; do
  [ -s "/usr/local/lib/usnm-boinc/well-known/$f" ] || { echo "missing .well-known/$f" >&2; exit 1; }
done
test -r /usr/local/lib/usnm-boinc/work-volume.py
python3 /usr/local/lib/usnm-boinc/work-volume.py --help >/dev/null
php -m | grep -qx mysqli
apache2ctl -M 2>/dev/null | grep -q php
python3 -c "import MySQLdb"
php -l html/inc/db.inc
php -l html/inc/db_conn.inc
test -r /usr/local/share/usnm/mysql-tls.cnf
test "$(grep -c "BEGIN CERTIFICATE" /usr/local/share/usnm/mysql-ca.pem)" = 2
openssl verify -CAfile /usr/local/share/usnm/mysql-ca.pem \
  /usr/share/ca-certificates/mozilla/DigiCert_Global_Root_G2.crt \
  /usr/share/ca-certificates/mozilla/Microsoft_RSA_Root_Certificate_Authority_2017.crt
# The signing tool must be here; the code-signing PRIVATE key must not be.
if find / -xdev -name code_sign_private 2>/dev/null | grep -q .; then echo "code_sign_private is in the image" >&2; exit 1; fi
# A fresh bridge container sees a dangling shared config link. Render only
# local hierarchy settings, then exercise the real pinned native helper.
mkdir gold-mirror-fixture
cd gold-mirror-fixture
mkdir -p project/cgi-bin project/download local
ln -s "$PWD/local/config.xml" project/config.xml
python3 /usr/local/lib/usnm-boinc/gold-mirror-config.py --project "$PWD/project" \
  --config "$PWD/local/config.xml" --template /usr/local/lib/usnm-boinc/config.xml.template
test "$(readlink project/config.xml)" = "$PWD/local/config.xml"
! grep -q "db_" local/config.xml
physical=usnm_gold_input_1bbcf8196839a292d2635a7ab7400430c0e00f90e0dae792c11c8495d1dde98e.jpg
actual=$(BOINC_PROJECT_DIR="$PWD/project" /usr/local/boinc/tools/dir_hier_path "$physical")
case "$actual" in "$PWD/project/download/"*"/$physical") ;; *) echo "unexpected native hierarchy path: $actual"; exit 1 ;; esac
test -d "$(dirname "$actual")"
before=$(sha256sum local/config.xml)
if python3 /usr/local/lib/usnm-boinc/gold-mirror-config.py --project "$PWD/project" \
  --config "$PWD/local/config.xml" --template /usr/local/lib/usnm-boinc/config.xml.template 2>/dev/null; then
  echo "mirror config must refuse an existing local target"; exit 1
fi
test "$before" = "$(sha256sum local/config.xml)"
echo "tools ok"'
out=$(mktemp -d)
chmod 777 "$out"
docker run --rm -v "$out:/out" "$image" --make-upload-keys --dir /out > /dev/null
[ -s "$out/upload_private" ] && [ -s "$out/upload_public" ] || { echo "make-upload-keys wrote no keys" >&2; exit 1; }
echo "upload keys ok"
