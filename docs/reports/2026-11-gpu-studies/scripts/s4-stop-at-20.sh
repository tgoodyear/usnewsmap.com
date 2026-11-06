#!/usr/bin/env bash
# Stop each Study 4 Surya job once both hold the same 20 pages: the rate is stable and the rest would only cost money.
# Rental endpoints must be a dotted-quad IPv4 address and a TCP port.
# SSH host keys must be pinned in GPU_STUDY_SECRETS/known_hosts before this watcher can connect.
cd "${GPU_STUDY_ROOT:?GPU_STUDY_ROOT is required}" || exit 2
valid_ip() {
  local a b c d o
  [[ $1 =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || return 1
  IFS=. read -r a b c d <<<"$1"
  for o in "$a" "$b" "$c" "$d"; do [ "$((10#$o))" -le 255 ] || return 1; done
}
valid_port() { [[ $1 =~ ^[0-9]{1,5}$ ]] && [ "$((10#$1))" -ge 1 ] && [ "$((10#$1))" -le 65535 ]; }
cnt() {
  local listing j endpoint IP PORT last status
  listing=$(scripts/vast.sh --action list) || { echo "failed to list rentals" >&2; return 2; }
  j=$(printf '%s\n' "$listing" | grep -F -- "$1")
  [ -n "$j" ] || { echo -1; return; }
  # Validate strings in jq before Bash can discard NUL bytes or trailing newlines.
  endpoint=$(printf '%s\n' "$j" | jq -ser '
    select(length == 1) | .[0] |
    .public_ipaddr as $ip | .ports["22/tcp"][0].HostPort as $port |
    select(($ip | type) == "string") |
    select(($port | type) == "string" or ($port | type) == "number") |
    select($ip | test("\\A[0-9]{1,3}(\\.[0-9]{1,3}){3}\\z")) |
    ($port | tostring) as $p | select($p | test("\\A[0-9]{1,5}\\z")) |
    [$ip, $p] | @tsv') || {
    echo "invalid SSH endpoint for $1" >&2; return 2
  }
  IFS=$'\t' read -r IP PORT <<<"$endpoint"
  if ! valid_ip "$IP" || ! valid_port "$PORT"; then echo "invalid SSH endpoint for $1" >&2; return 2; fi
  [ -n "${GPU_STUDY_SECRETS:-}" ] || { echo "GPU_STUDY_SECRETS is required" >&2; return 2; }
  local known_hosts="$GPU_STUDY_SECRETS/known_hosts"
  [ -f "$known_hosts" ] && [ -r "$known_hosts" ] && [ ! -L "$known_hosts" ] || {
    echo "trusted GPU rental host keys are required in GPU_STUDY_SECRETS/known_hosts" >&2; return 2;
  }
  last=$(
    set -o pipefail
    env -u SSH_AUTH_SOCK ssh -F /dev/null -n -p "$PORT" -i "$GPU_STUDY_SECRETS/rental_ed25519" \
      -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes \
      -o "UserKnownHostsFile=$known_hosts" -o ConnectTimeout=15 "root@$IP" "$2" 2>/dev/null | tail -1
  )
  status=$?
  if [ "$status" -ne 0 ]; then echo "SSH output pipeline failed for $1 (status $status)" >&2; return "$status"; fi
  [ -z "$last" ] || printf '%s\n' "$last"
}
while :; do
  for k in s4surya3090 s4surya4090; do n=$(cnt "$k" 'wc -l < /work/out/surya-pages.jsonl') || exit $?; echo "$(date -Is) $k $n" >> s4watch.log
    if [ "${n:-0}" -ge 20 ]; then cnt "$k" 'pkill -f surya_read.py' || exit $?; fi; done
  listing=$(scripts/vast.sh --action list) || { echo "failed to list rentals" >&2; exit 2; }
  printf '%s\n' "$listing" | grep -q s4surya || break
  sleep 120
done
echo S4-WATCH-DONE >> s4watch.log
