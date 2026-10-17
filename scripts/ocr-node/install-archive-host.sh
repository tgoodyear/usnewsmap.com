#!/usr/bin/env bash
# Install attestation-verified inputs staged by the operator on the scan host.
#   install-archive-host.sh --dir DIR --release TAG --binary-sha256 HEX \
#     --archive-url https://ACCOUNT.blob.core.windows.net/scans \
#     [--set keep|buffer|both] [--evict] [--on-calendar SPEC] \
#     [--seed-done ARCHIVED_TSV] [--dry-run]
# Does not start uploads or enable timers. Binary attestation must be checked
# by the operator before staging; the supplied digest is rechecked here.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
DIR="" RELEASE="" SHA="" URL="" SET="" EVICT=false ON_CALENDAR="" SEED_DONE="" DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR=${2:-}; shift 2 ;;
    --release) RELEASE=${2:-}; shift 2 ;;
    --binary-sha256) SHA=${2:-}; shift 2 ;;
    --archive-url) URL=${2:-}; shift 2 ;;
    --set) SET=${2:-}; shift 2 ;;
    --evict) EVICT=true; shift ;;
    --on-calendar) ON_CALENDAR=${2:-}; shift 2 ;;
    --seed-done) SEED_DONE=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) die "unknown option $1" ;;
  esac
done
[ "$(id -u)" = 0 ] || die "run as root on the scan host"
[[ $RELEASE =~ ^ocr-worker-[0-9]{8}-[0-9a-f]{7}$ ]] || die "invalid release"
[[ $SHA =~ ^[0-9a-f]{64}$ ]] || die "invalid verified binary digest"
[[ $URL =~ ^https://[a-z0-9]{3,24}\.blob\.core\.windows\.net/scans$ ]] || die "invalid archive container URL"
[ -z "$SET" ] || [[ $SET =~ ^(keep|buffer|both)$ ]] || die "invalid --set ($SET): expected keep, buffer, or both"
[ -z "$SEED_DONE" ] || [ -f "$SEED_DONE" ] || die "missing --seed-done file $SEED_DONE"
[ -d "$DIR" ] && [ ! -L "$DIR" ] || die "missing staging directory"
[ -d /tank/ocr/scans/keep ] || die "keep scans are not mounted"
getent group himds > /dev/null || die "missing Arc token group"
getent group ocrdata > /dev/null || die "missing scan read group"
for f in usnm-ocr-worker usnm-scan-archive.service usnm-scan-archive.timer \
  usnm-scan-archive-verify.service usnm-scan-archive-verify.timer \
  usnm-scan-archive.conf check-scan-archive.sh; do
  [ -f "$DIR/$f" ] && [ ! -L "$DIR/$f" ] || die "missing staged regular file $f"
done
[ "$(sha256sum "$DIR/usnm-ocr-worker" | cut -d' ' -f1)" = "$SHA" ] || die "binary digest differs"
if [ "$DRY_RUN" = true ]; then
  echo "validated $RELEASE ($SHA); would install archive-only user/binary/units; no upload or timer start"
  exit 0
fi
if [ -e /etc/usnm/scan-archive.env ]; then
  grep -Fxq "USNM_ARCHIVE_URL=$URL" /etc/usnm/scan-archive.env ||
    die "existing archive environment differs; refusing to overwrite"
fi
if systemctl is-active --quiet usnm-ocr-worker.service; then
  die "OCR worker is active on this host; this installer is for the separate scan host"
fi
for unit in usnm-scan-archive.service usnm-scan-archive-verify.service; do
  if systemctl is-active --quiet "$unit"; then
    die "$unit is active; do not replace its inputs during an archive operation"
  fi
done
command -v jq > /dev/null || {
  command -v apt-get > /dev/null || die "install jq before continuing"
  apt-get update -qq
  apt-get install -y -qq jq
}
getent group usnm > /dev/null || groupadd --system usnm
if ! id usnm > /dev/null 2>&1; then
  useradd --system --gid usnm --home-dir /var/lib/usnm \
    --no-create-home --shell /usr/sbin/nologin usnm
fi
usermod -a -G himds,ocrdata usnm
runuser -u usnm -- test -r /tank/ocr/scans/keep || die "service user cannot read keep scans"
install -d -m 0755 "/usr/local/lib/usnm-ocr-worker/$RELEASE" /etc/usnm \
  /usr/local/libexec/usnm-scan-archive
install -m 0755 "$DIR/usnm-ocr-worker" "/usr/local/lib/usnm-ocr-worker/$RELEASE/usnm-ocr-worker"
ln -sfn "/usr/local/lib/usnm-ocr-worker/$RELEASE/usnm-ocr-worker" /usr/local/bin/usnm-ocr-worker
install -m 0755 "$DIR/check-scan-archive.sh" /usr/local/libexec/usnm-scan-archive/check
install -m 0644 "$DIR/usnm-scan-archive.conf" /etc/tmpfiles.d/
systemd-tmpfiles --create /etc/tmpfiles.d/usnm-scan-archive.conf
for f in usnm-scan-archive.service usnm-scan-archive.timer \
  usnm-scan-archive-verify.service usnm-scan-archive-verify.timer; do
  install -m 0644 "$DIR/$f" /etc/systemd/system/
done
ENVFILE=/etc/usnm/scan-archive.env
if [ ! -e "$ENVFILE" ]; then
  (umask 027; printf 'USNM_ARCHIVE_URL=%s\n' "$URL" > "$ENVFILE")
  chown root:usnm "$ENVFILE"
fi
if [ -n "$SET" ]; then
  grep -v '^USNM_ARCHIVE_SET=' "$ENVFILE" > "$ENVFILE.tmp" || true
  printf 'USNM_ARCHIVE_SET=%s\n' "$SET" >> "$ENVFILE.tmp"
  mv "$ENVFILE.tmp" "$ENVFILE"
fi
if [ "$EVICT" = true ]; then
  grep -v '^USNM_ARCHIVE_EVICT=' "$ENVFILE" > "$ENVFILE.tmp" || true
  printf 'USNM_ARCHIVE_EVICT=true\n' >> "$ENVFILE.tmp"
  mv "$ENVFILE.tmp" "$ENVFILE"
  chown root:ocrdata "$ENVFILE"
  chmod 0640 "$ENVFILE"
  install -d -m 0755 /etc/systemd/system/usnm-scan-archive.service.d \
    /var/lib/usnm-scan-archive
  chown -R ocrdata:ocrdata /var/lib/usnm-scan-archive
  cat > /etc/systemd/system/usnm-scan-archive.service.d/spool.conf <<'EOF'
[Service]
User=ocrdata
Group=ocrdata
SupplementaryGroups=himds usnm
ReadWritePaths=/run/lock/usnm-scan-archive /tank/ocr/scans
EOF
fi
if [ -n "$ON_CALENDAR" ]; then
  install -d -m 0755 /etc/systemd/system/usnm-scan-archive.timer.d
  cat > /etc/systemd/system/usnm-scan-archive.timer.d/override.conf <<EOF
[Timer]
OnCalendar=
OnCalendar=$ON_CALENDAR
RandomizedDelaySec=1min
Persistent=true
EOF
fi
if [ -n "$SEED_DONE" ]; then
  install -d -m 0755 /var/lib/usnm-scan-archive
  cp "$SEED_DONE" /var/lib/usnm-scan-archive/archived.tsv
  if [ "$EVICT" = true ]; then
    chown ocrdata:ocrdata /var/lib/usnm-scan-archive/archived.tsv
  else
    chown usnm:usnm /var/lib/usnm-scan-archive/archived.tsv
  fi
  python3 - "$SEED_DONE" /tank/ocr/scans <<'PY'
import os
import sys

tsv_path, scans_root = sys.argv[1], sys.argv[2]
keep_root = os.path.join(scans_root, "keep")
seeded = 0
with open(tsv_path, "r", encoding="utf-8") as fh:
    for line in fh:
        rel = line.split("\t", 1)[0].strip()
        if not rel or ".." in rel:
            continue
        target = os.path.join(scans_root, rel) if rel.startswith(("keep/", "buffer/")) else os.path.join(keep_root, rel)
        done = target + ".done"
        os.makedirs(os.path.dirname(done), exist_ok=True)
        if not os.path.exists(done):
            open(done, "wb").close()
            seeded += 1
print(f"seeded {seeded} .done markers from {tsv_path}")
PY
  chown -R ocrdata:ocrdata /tank/ocr/scans/keep
  [ ! -d /tank/ocr/scans/buffer ] || chown -R ocrdata:ocrdata /tank/ocr/scans/buffer
fi
systemd-analyze verify /etc/systemd/system/usnm-scan-archive.service \
  /etc/systemd/system/usnm-scan-archive-verify.service
systemctl daemon-reload
echo "archive installed: $RELEASE; run /usr/local/libexec/usnm-scan-archive/check --limit 100"
echo "no upload or timer was started"
