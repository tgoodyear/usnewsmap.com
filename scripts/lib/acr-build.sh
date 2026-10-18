#!/usr/bin/env bash
# acr_build_verified AZ_COMMAND REGISTRY REPOSITORY:TAG BUILD_TIMEOUT [build arguments...]
# AZ_COMMAND may be a function (e.g. baz) that supplies the caller's subscription.
# Queue exactly once; only the canonical run status and its pushed digest prove success.
acr_build_verified() (
  local cli=$1 registry=$2 image=$3 build_timeout=$4
  shift 4
  local run_id submit_status=0 run status expected actual
  local wait_seconds=${ACR_WAIT_SECONDS:-$((build_timeout + 600))}
  local interval=${ACR_POLL_INTERVAL:-10} deadline remaining
  [[ $wait_seconds =~ ^[0-9]+$ && $interval =~ ^[0-9]+$ ]] || {
    echo "error: ACR_WAIT_SECONDS and ACR_POLL_INTERVAL must be nonnegative seconds" >&2
    return 1
  }
  run_id=$("$cli" acr build --registry "$registry" --image "$image" --timeout "$build_timeout" \
    --no-wait --query runId -o tsv "$@") || submit_status=$?
  # A nonzero client exit with an acknowledged run is not a remote build failure (#549).
  # Never guess a run from the latest registry run or an old tag when submission is ambiguous.
  [[ $run_id =~ ^[A-Za-z0-9]+$ && $run_id != null && $run_id != None ]] || {
    echo "error: ACR submission returned no valid run ID (CLI exit $submit_status); not retrying" >&2
    return 1
  }
  echo "ACR $registry run $run_id: queued $image (CLI exit $submit_status)" >&2
  deadline=$((SECONDS + wait_seconds))
  while :; do
    if run=$("$cli" acr task show-run --registry "$registry" --run-id "$run_id" -o json) &&
      status=$(jq -er '.status | select(type == "string")' <<< "$run"); then
      case "$status" in
        Succeeded) break ;;
        Failed|Canceled|Cancelled|Error|Timeout)
          echo "error: ACR $registry run $run_id: $status" >&2
          return 1 ;;
        Queued|Started|Running) ;;
        *) echo "error: ACR $registry run $run_id: unexpected status $status" >&2; return 1 ;;
      esac
    else
      echo "warning: could not read ACR $registry run $run_id; retrying within deadline" >&2
    fi
    remaining=$((deadline - SECONDS))
    if [ "$remaining" -le 0 ]; then
      echo "error: timed out waiting for ACR $registry run $run_id; remote run may still be active" >&2
      return 1
    fi
    sleep "$((interval < remaining ? interval : remaining))"
  done
  expected=$(jq -er --arg repo "${image%:*}" --arg tag "${image##*:}" \
    '[.outputImages[]? | select(.repository == $repo and .tag == $tag) | .digest] |
     if length == 1 then .[0] else empty end' <<< "$run") || {
    echo "error: ACR $registry run $run_id did not report a digest for $image" >&2
    return 1
  }
  [[ $expected =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo "error: ACR $registry run $run_id reported an invalid digest for $image" >&2
    return 1
  }
  actual=$("$cli" acr repository show --name "$registry" --image "$image" --query digest -o tsv) || {
    echo "error: cannot verify $registry/$image after ACR run $run_id" >&2
    return 1
  }
  [ "$actual" = "$expected" ] || {
    echo "error: digest mismatch for $registry/$image after ACR run $run_id" >&2
    return 1
  }
  echo "ACR $registry run $run_id: Succeeded; verified $image@$actual" >&2
)
