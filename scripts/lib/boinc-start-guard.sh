# shellcheck shell=bash disable=SC2154
# Cross-host mutex for a launcher's check/update/start/read-back transaction.
# Uses the existing exchange account; independent of the controller's tick lease
# so controller-launched child scripts never recursively acquire that lease.
#
# Every az call here is bounded (scripts/lib/bounded-az.sh), because a hung az would hold the
# lease with nobody to end it. Reads are retried. The lease and storage writes get a timeout and
# no retry: a request that timed out may have taken effect, so it is handled as an unknown
# outcome. The confirm and reconcile loops poll within a window (BOINC_START_CONFIRM_SECS and
# BOINC_START_RECONCILE_SECS, 120 seconds each), and their reads are not retried past it.
# shellcheck source=scripts/lib/bounded-az.sh
. "$(dirname "${BASH_SOURCE[0]}")/bounded-az.sh"
BOINC_START_LEASE=""
BOINC_START_WORK=""
BOINC_START_SAFE_TO_RELEASE=true

# boinc_start_blob baz_ro|baz_mut ARG...: one az storage blob call, bounded as the first word says.
boinc_start_blob() {
  local bounded=$1
  shift
  "$bounded" storage blob "$@" --auth-mode login --account-name "$BOINC_START_ACCOUNT" \
    --container-name boinc-handoff
}

boinc_start_guard_acquire() {
  local outputs exists candidate rc=0
  outputs=$(baz_ro stack sub show --name "usnm-$ENV_NAME" --query outputs -o json) ||
    die "can't discover the exchange account for the cross-host start guard"
  BOINC_START_ACCOUNT=$(jq -er 'to_entries[] | select((.key | ascii_downcase) == "ocr_exchange_account") | .value.value' <<< "$outputs") ||
    die "the main stack has no OCR_EXCHANGE_ACCOUNT; cannot safely start $job"
  [[ $BOINC_START_ACCOUNT =~ ^[a-z0-9]{3,24}$ ]] || die "invalid exchange account for the cross-host start guard"
  BOINC_START_BLOB="scheduler/starts/$(printf '%s/%s' "$JOB_RG" "$job" | tr '[:upper:]' '[:lower:]').lock"
  exists=$(boinc_start_blob baz_ro exists --name "$BOINC_START_BLOB" --query exists -o tsv) ||
    die "can't read the cross-host start guard for $job"
  if [ "$exists" != true ]; then
    BOINC_START_WORK=$(python3 - <<'PY'
from pathlib import Path
import uuid
work = Path.cwd() / (".boinc-start-" + uuid.uuid4().hex)
work.mkdir(mode=0o700)
(work / "marker.json").write_text('{"schema_version":1}\n')
print(work)
PY
    ) || die "can't prepare the cross-host start guard"
    # Two first callers may both observe no blob. Never overwrite the winner.
    if ! boinc_start_blob baz_mut upload --name "$BOINC_START_BLOB" --file "$BOINC_START_WORK/marker.json" --overwrite false -o none; then
      [ "$(boinc_start_blob baz_ro exists --name "$BOINC_START_BLOB" --query exists -o tsv)" = true ] ||
        die "can't initialize the cross-host start guard for $job"
    fi
  fi
  candidate=$(python3 -c 'import uuid; print(uuid.uuid4())') || die "can't generate a start lease ID"
  boinc_start_blob baz_mut lease acquire --blob-name "$BOINC_START_BLOB" --lease-duration -1 \
    --proposed-lease-id "$candidate" -o none || rc=$?
  # A timed-out acquire may have taken the lease: BOINC_START_LEASE stays empty (the cleanup must
  # not release a lease that may be another launcher's), and the error names what to reconcile.
  [ "$rc" -ne 124 ] ||
    die "timed out acquiring the cross-host start guard for $job; the lease may have been taken. If $BOINC_START_BLOB (container boinc-handoff) is held with lease ID $candidate, that was this launcher: release it with that ID once nothing else is starting $job"
  [ "$rc" -eq 0 ] ||
    die "can't acquire the cross-host start guard for $job; another launcher may own it (never break a live lease)"
  BOINC_START_LEASE=$candidate
}

# Keep the lease until a subsequent caller can see the active execution in list,
# or the execution has already ended. A failed/ambiguous check leaves the infinite
# lease in place for operator reconciliation rather than allowing another start.
boinc_start_guard_confirm() {
  local status visible deadline window=${BOINC_START_CONFIRM_SECS:-120}
  local BAZ_RO_UNTIL=$((SECONDS + window))
  deadline=$(($(date +%s) + window))
  while :; do
    status=$(baz_ro containerapp job execution show --name "$job" --resource-group "$JOB_RG" \
      --job-execution-name "$execution" --query properties.status -o tsv) || return 1
    case "$status" in Succeeded | Failed | Stopped | Degraded) BOINC_START_SAFE_TO_RELEASE=true; return 0 ;; esac
    visible=$(baz_ro containerapp job execution list --name "$job" --resource-group "$JOB_RG" \
      --query "[?name=='$execution' && properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) || return 1
    [ "$visible" != "$execution" ] || { BOINC_START_SAFE_TO_RELEASE=true; return 0; }
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    sleep 5
  done
}

# After a failed launch step the outcome is unknown. BOINC_START_UNCERTAIN says which step:
# - start: the execution start, or its read-back. A known execution ($execution, the start
#   returned a name) is released once the job's list shows it, so the next launcher's
#   running-executions check refuses while it runs. With none known, it is released once the
#   list has been read for the window boinc_start_guard_confirm allows a new execution to
#   appear in.
# - update: the job's template update. As for start, and only while the job's
#   provisioningState is terminal: an update still in flight could change what the next
#   launcher starts.
# - deploy: a stack deployment, which can still rewrite job templates after the CLI fails, and
#   which execution listings cannot see. Never released here.
# A failed read, an update not yet terminal at the end of the window, or a known execution
# still missing from the list leaves the outcome unknown and the lease held.
BOINC_START_UNCERTAIN=""
boinc_start_guard_reconcile() {
  local deadline names state rg=${JOB_RG:-${BOINC_RG:-}} window=${BOINC_START_RECONCILE_SECS:-120}
  local BAZ_RO_UNTIL=$((SECONDS + window))
  [ "${BOINC_START_UNCERTAIN:-start}" != deploy ] || return 1
  [ -n "${job:-}" ] && [ -n "$rg" ] || return 1
  deadline=$(($(date +%s) + window))
  while :; do
    if [ "$BOINC_START_UNCERTAIN" = update ]; then
      state=$(baz_ro containerapp job show --name "$job" --resource-group "$rg" \
        --query properties.provisioningState -o tsv) || return 1
    else
      state=Succeeded
    fi
    names=$(baz_ro containerapp job execution list --name "$job" --resource-group "$rg" --query "[].name" -o tsv) ||
      return 1
    case "$state" in
      Succeeded | Failed | Canceled)
        if [ -n "${execution:-}" ] && grep -qxF "$execution" <<< "$names"; then return 0; fi ;;
    esac
    if [ "$(date +%s)" -ge "$deadline" ]; then
      case "$state" in Succeeded | Failed | Canceled) ;; *) return 1 ;; esac
      # Explicit statuses: this runs from the EXIT trap, where a bare `return` (bash 4+)
      # returns the status that triggered the trap, not the last command's.
      if [ -n "${execution:-}" ]; then return 1; fi
      return 0
    fi
    sleep "${BOINC_START_RECONCILE_POLL:-10}"
  done
}

boinc_start_guard_release() {
  local failed=0
  if [ -n "$BOINC_START_LEASE" ]; then
    if [ "$BOINC_START_SAFE_TO_RELEASE" != true ] && boinc_start_guard_reconcile; then
      echo "warning: template update/start outcome was uncertain; $job's executions listed for the settling window, releasing the cross-host lease" >&2
      BOINC_START_SAFE_TO_RELEASE=true
    fi
    if [ "$BOINC_START_SAFE_TO_RELEASE" = true ]; then
      boinc_start_blob baz_mut lease release --blob-name "$BOINC_START_BLOB" --lease-id "$BOINC_START_LEASE" -o none || failed=1
      [ "$failed" -eq 0 ] ||
        echo "error: cross-host start lease release failed for $job; reconcile it before another launch ($BOINC_START_BLOB is held with lease ID $BOINC_START_LEASE unless the release took effect)" >&2
    else
      echo "error: template update/start outcome uncertain; cross-host lease retained for operator reconciliation ($BOINC_START_BLOB)" >&2
      failed=1
    fi
    BOINC_START_LEASE=""
  fi
  if [ -n "$BOINC_START_WORK" ]; then
    rm -rf "$BOINC_START_WORK"
    BOINC_START_WORK=""
  fi
  return "$failed"
}

boinc_start_guard_cleanup() {
  local result=$1
  boinc_start_guard_release || result=1
  if [ "$TARGET" = bridge ]; then boinc_release_lock; fi
  return "$result"
}
