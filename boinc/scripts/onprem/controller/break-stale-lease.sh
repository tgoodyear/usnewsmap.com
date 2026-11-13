#!/usr/bin/env bash
# Breaks the controller's hand-off lease left held by a tick that died mid-run (#1698).
#
# Transport.lock takes an infinite lease on boinc-handoff/scheduler/controller.lock. A CT stop,
# move or power cycle in the middle of a tick leaves it held, and every plan's tick then fails
# until someone breaks it by hand. Run at boot, before any tick can start, no batch_scheduler
# process exists, so a lease still held is stale. The long-term fix is a finite lease that a
# running tick renews (#1698); this closes the gap until then.
#
#   break-stale-lease.sh [--account NAME] [--container NAME] [--blob NAME]
#                        [--attempts N] [--delay SECONDS]
#
# Exits 0 when the blob is not leased or the lease was broken, 1 when a batch_scheduler process
# is running (nothing is touched) or the break still fails after every attempt. Azure sign-in
# (the CT's Arc identity) may not be ready at boot, so a failed attempt is retried.
set -uo pipefail
ACCOUNT=stusnmoffapzy
CONTAINER=boinc-handoff
BLOB=scheduler/controller.lock
ATTEMPTS=16
DELAY=15
die() { echo "error: $*" >&2; exit 2; }
log() { echo "break-stale-lease: $*"; }
while [ $# -gt 0 ]; do
  [ $# -ge 2 ] || die "$1 needs a value"
  case $1 in
    --account) ACCOUNT=$2 ;;
    --container) CONTAINER=$2 ;;
    --blob) BLOB=$2 ;;
    --attempts) ATTEMPTS=$2 ;;
    --delay) DELAY=$2 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift 2
done

if pgrep -f batch_scheduler >/dev/null; then
  log "a batch_scheduler process is running; its lease is not stale, nothing done"
  exit 1
fi

for attempt in $(seq 1 "$ATTEMPTS"); do
  out=$(az storage blob lease break --auth-mode login --account-name "$ACCOUNT" \
    --container-name "$CONTAINER" --blob-name "$BLOB" --lease-break-period 0 2>&1)
  rc=$?
  if [ $rc -eq 0 ]; then
    log "broke the lease on $CONTAINER/$BLOB (attempt $attempt)"
    exit 0
  fi
  # Azure answers 409 LeaseNotPresentWithLeaseOperation for an unleased blob: nothing to do.
  if grep -q -E "LeaseNotPresent|There is currently no lease" <<<"$out"; then
    log "no lease on $CONTAINER/$BLOB; nothing to break (attempt $attempt)"
    exit 0
  fi
  log "attempt $attempt of $ATTEMPTS failed: $(tr '\n' ' ' <<<"$out" | cut -c1-300)"
  [ "$attempt" -lt "$ATTEMPTS" ] && sleep "$DELAY"
done
log "gave up on $CONTAINER/$BLOB; the lease may still be held (#1698)"
exit 1
