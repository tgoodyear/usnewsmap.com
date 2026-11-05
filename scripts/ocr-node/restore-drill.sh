#!/usr/bin/env bash
# Restore drill for the scan archive (#895): restore a small spread sample
# from the Cold archive and from a ZFS snapshot, then check every copy.
#   restore-drill.sh --dir DIR --dataset POOL/NAME --snapshot NAME
#     [--count 20] [--budget-bytes 100000000]
# Reads only: HEAD/GET on the archive, the snapshot and the live scans. Writes
# only under DIR, which must be the empty mountpoint of the scratch DATASET.
# Never prints blob names or the token.
set -euo pipefail
export LC_ALL=C
die() { echo "error: $*" >&2; exit 1; }
DIR="" DATASET="" SNAP="" COUNT=20 BUDGET=100000000
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR=${2:-}; shift 2 ;;
    --dataset) DATASET=${2:-}; shift 2 ;;
    --snapshot) SNAP=${2:-}; shift 2 ;;
    --count) COUNT=${2:-}; shift 2 ;;
    --budget-bytes) BUDGET=${2:-}; shift 2 ;;
    -h|--help) sed -n '2,8s/^# \{0,1\}//p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $DIR =~ ^/[A-Za-z0-9._/-]+$ ]] || die "--dir must be a plain absolute path"
[[ $DATASET =~ ^[A-Za-z0-9_.-]+(/[A-Za-z0-9_.:-]+)+$ ]] || die "--dataset must name a child dataset, such as tank/usnm-restore-drill-20261105"
[[ $SNAP =~ ^[A-Za-z0-9_.:-]+$ ]] ||
  die "--snapshot must be the name after @ of a scans snapshot, such as autosnap_2026-11-05_00:00:03_daily"
[[ $COUNT =~ ^[1-9][0-9]?$ ]] || die "--count must be 1 to 99 scans"
[[ $BUDGET =~ ^[1-9][0-9]{0,8}$ ]] && [ "$BUDGET" -le 100000000 ] ||
  die "--budget-bytes must be 1 to 100000000"
for cmd in awk base64 cmp curl findmnt ionice jq md5sum od renice sha256sum stat; do
  command -v "$cmd" > /dev/null || die "needs $cmd"
done
ionice -c 3 -p $$ > /dev/null
renice -n 19 -p $$ > /dev/null

ROOT=${USNM_OCR_TEST_ROOT:-}
ENVFILE=$ROOT/etc/usnm/scan-archive.env
MANIFEST=$ROOT/var/lib/usnm-scan-archive/archived.tsv
KEYS=$ROOT/var/opt/azcmagent/tokens
# 127.0.0.1, not localhost: curl tries ::1 first, and himds can accept a
# connection there without ever answering it.
TOKEN_URL=${USNM_OCR_TEST_TOKEN_URL:-http://127.0.0.1:40342/metadata/identity/oauth2/token?api-version=2019-11-01&resource=https%3A%2F%2Fstorage.azure.com%2F}
TOKEN_TIMEOUT=${USNM_OCR_TEST_TOKEN_TIMEOUT:-30}
[[ $TOKEN_URL =~ ^http://127[.]0[.]0[.]1:([1-9][0-9]{0,4})/ ]] && [ "${BASH_REMATCH[1]}" -le 65535 ] ||
  die "the token endpoint must be http://127.0.0.1:<port 1-65535>/, never localhost"
[[ $TOKEN_TIMEOUT =~ ^[1-9][0-9]{0,2}$ ]] || die "the token timeout must be 1 to 999 seconds"
[ -r "$ENVFILE" ] || die "missing /etc/usnm/scan-archive.env"
[ -r "$MANIFEST" ] || die "missing the upload's archived.tsv"
setting() {
  local v
  v=$(sed -n "s/^$1=//p" "$ENVFILE" | tail -n 1)
  if [[ $v =~ ^\"(.*)\"$ || $v =~ ^\'(.*)\'$ ]]; then v=${BASH_REMATCH[1]}; fi
  printf '%s' "$v"
}
URL=$(setting USNM_ARCHIVE_URL)
[[ $URL =~ ^(https://[A-Za-z0-9.-]+|http://127[.]0[.]0[.]1:[0-9]{1,5})/[a-z0-9-]{3,63}$ ]] ||
  die "unexpected USNM_ARCHIVE_URL"
SCANS=$(setting USNM_ARCHIVE_SCANS)
SCANS=$ROOT${SCANS:-/tank/ocr/scans}
SNAPDIR=$SCANS/.zfs/snapshot/$SNAP
[ "$(findmnt -rn -o SOURCE,FSTYPE --mountpoint "$DIR")" = "$DATASET zfs" ] ||
  die "$DIR is not the mountpoint of $DATASET"
[ -z "$(ls -A "$DIR")" ] || die "$DIR is not empty"
[ -d "$SNAPDIR/keep" ] || die "snapshot $SNAP has no keep set"

now() { date +%s.%N; }
secs() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.2f", b - a }'; }

# Spread over the manifest, offset half a step from check-scan-archive.sh's sample.
TOTAL=$(awk 'END { print NR }' "$MANIFEST")
mapfile -t CAND < <(awk -F'\t' -v n="$COUNT" -v t="$TOTAL" '
  BEGIN { for (i = 0; i < n; i++) pick[int((i + 0.5) * t / n) + 1] = 1 }
  NR in pick { print $1 }' "$MANIFEST")
[ "${#CAND[@]}" -gt 0 ] || die "the manifest is empty"
for name in "${CAND[@]}"; do
  [[ $name =~ ^keep(/[A-Za-z0-9_][A-Za-z0-9._-]*)+[.](jp2|jpg)$ ]] || die "unexpected name in the manifest"
done

# The token stays in this shell: headers reach curl on stdin, never argv.
# The first Basic challenge; its realm may be quoted and may come with other parameters.
challenge=$(curl -s --max-time "$TOKEN_TIMEOUT" -o /dev/null -D - -H Metadata:true "$TOKEN_URL" |
  tr -d '\r' | awk 'tolower($0) ~ /^www-authenticate:[ \t]*basic[ \t]/ && !n++ { sub(/^[^:]*:[ \t]*/, ""); print }') ||
  die "token challenge failed"
re='[[:space:],]realm=("([^"]*)"|([^",[:space:]]*))'
[[ $challenge =~ $re ]] || die "unexpected token challenge"
realm=${BASH_REMATCH[2]:-${BASH_REMATCH[3]}}
[[ $realm == "$KEYS"/* && ${realm#"$KEYS"/} =~ ^[A-Za-z0-9-]+[.]key$ ]] || die "unexpected token challenge"
[ -f "$realm" ] && [ "$(stat -c %s "$realm")" -le 4096 ] || die "unexpected token key file"
t0=$(now)
TOKEN=$(printf 'header = "Authorization: Basic %s"\n' "$(< "$realm")" |
  curl -sf --max-time "$TOKEN_TIMEOUT" -K - -H Metadata:true "$TOKEN_URL" | jq -r .access_token) ||
  die "token request failed"
[ -n "$TOKEN" ] && [ "$TOKEN" != null ] || die "no token"
echo "== token in $(secs "$t0" "$(now)")s"
blob() {
  printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    curl -sS -f --connect-timeout 20 -K - -H 'x-ms-version: 2021-08-06' "$@"
}
hdr() { tr -d '\r' < "$1" | awk -F': ' -v k="$2" 'tolower($1) == k { print $2 }' | tail -n 1; }

# HEAD in order; stop at the first scan that would take the GETs past the budget.
mkdir -m 0700 "$DIR/head" "$DIR/cold" "$DIR/snap"
SEL=() SIZES=() spent=0
for i in "${!CAND[@]}"; do
  blob -I -o "$DIR/head/$i" --max-time 30 "$URL/${CAND[$i]}" || die "HEAD $i failed"
  len=$(hdr "$DIR/head/$i" content-length)
  [[ $len =~ ^[0-9]+$ ]] || die "HEAD $i returned no length"
  if [ $((spent + len)) -gt "$BUDGET" ]; then
    echo "== budget: stopping before candidate $i ($spent + $len > $BUDGET bytes)"
    break
  fi
  SEL+=("${CAND[$i]}") SIZES+=("$len") spent=$((spent + len))
done
N=${#SEL[@]}
[ "$N" -gt 0 ] || die "no scan fits the budget"
DIGEST=$(printf '%s\n' "${SEL[@]}" | sha256sum | cut -c1-16)
echo "== sample: $N of $COUNT spread over $TOTAL manifest lines; $spent bytes to read from Cold; names sha256 $DIGEST"

# Cold restore: sequential GETs, each capped at its HEAD length.
g0=$(now) got=0
for i in "${!SEL[@]}"; do
  w=$(blob --max-time 120 --max-filesize "${SIZES[$i]}" -o "$DIR/cold/$i" \
    -w '%{time_total} %{size_download}' "$URL/${SEL[$i]}") || die "GET $i failed"
  got=$((got + ${w#* }))
  [ "$got" -le "$BUDGET" ] || die "read past the budget"
  echo "   get $i: ${w#* } bytes in ${w% *}s"
done
g1=$(now)
unset TOKEN
echo "== cold restore: $N scans, $got bytes in $(secs "$g0" "$g1")s ($(awk -v b="$got" -v s="$(secs "$g0" "$g1")" 'BEGIN { printf "%.1f", s > 0 ? b / 1e6 / s : 0 }') MB/s)"

# Snapshot restore of the same scans; one archived after the snapshot is skipped.
s0=$(now) insnap=0
for i in "${!SEL[@]}"; do
  if [ -f "$SNAPDIR/${SEL[$i]}" ]; then
    cp -- "$SNAPDIR/${SEL[$i]}" "$DIR/snap/$i"
    insnap=$((insnap + 1))
  fi
done
echo "== snapshot restore ($SNAP): $insnap of $N scans in $(secs "$s0" "$(now)")s"

# Every restored copy against the sidecar, the blob's properties and the live scan.
v0=$(now) bad=0 nomd5=0
for i in "${!SEL[@]}"; do
  live="$SCANS/${SEL[$i]}" side="$SCANS/${SEL[$i]}.json" cold="$DIR/cold/$i" fails=() note=""
  r_sha=$(sha256sum < "$cold" | cut -d' ' -f1)
  r_md5=$(md5sum < "$cold" | cut -d' ' -f1)
  r_len=$(stat -c %s "$cold")
  b_md5=$(hdr "$DIR/head/$i" content-md5 | base64 -d | od -An -tx1 | tr -d ' \n')
  [ "$r_len" = "${SIZES[$i]}" ] || fails+=(length)
  [ "$r_len" = "$(jq -r .bytes "$side")" ] || fails+=(sidecar-bytes)
  [ "$r_sha" = "$(jq -r .sha256 "$side")" ] || fails+=(sidecar-sha256)
  # Only a JP2's sidecar records the bag's md5 (prefetch::matches_sidecar).
  s_md5=$(jq -r '.md5 // empty' "$side")
  if [ -n "$s_md5" ]; then
    [ "$r_md5" = "$s_md5" ] || fails+=(sidecar-md5)
  elif [ "$(jq -r '.format // "jpeg"' "$side")" = jp2 ]; then
    fails+=(sidecar-md5-missing)
  else
    nomd5=$((nomd5 + 1))
  fi
  [ "$r_sha" = "$(hdr "$DIR/head/$i" x-ms-meta-sha256)" ] || fails+=(blob-sha256)
  [ "$r_md5" = "$b_md5" ] || fails+=(blob-md5)
  [ "$(hdr "$DIR/head/$i" x-ms-access-tier)" = Cold ] || fails+=(tier)
  [ "$r_sha" = "$(sha256sum < "$live" | cut -d' ' -f1)" ] || fails+=(live-sha256)
  cmp -s "$cold" "$live" || fails+=(live-bytes)
  if [ -f "$DIR/snap/$i" ]; then
    cmp -s "$cold" "$DIR/snap/$i" || fails+=(snapshot-bytes)
  else
    note=", not in the snapshot"
  fi
  if [ "${#fails[@]}" -eq 0 ]; then
    echo "   check $i: ok ($r_len bytes$note)"
  else
    bad=$((bad + 1))
    echo "   check $i: FAILED ${fails[*]}"
  fi
done
echo "== verify: $((N - bad)) of $N match (snapshot copies: $insnap; JPEGs without a sidecar md5: $nomd5) in $(secs "$v0" "$(now)")s"
du -sh --apparent-size "$DIR" | cut -f1 | sed 's/^/== scratch used: /'
[ "$bad" -eq 0 ] || die "$bad restored scans did not match"
echo "restore drill passed: $N scans (sample $DIGEST)"
