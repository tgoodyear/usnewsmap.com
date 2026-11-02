#!/usr/bin/env bash
# Paths are environment variables: GPU_STUDY_ROOT, GPU_STUDY_DEV, SCAN_ROOT, SCAN_MANIFESTS, LOC_ARCHIVES, GPU_STUDY_SECRETS.
# Run one job on one rented GPU, then destroy the instance whatever happened.
# Hosts are untrusted: they get --push-dir and --job and nothing else. Results come back as plain
# files with checked names and capped sizes, into an empty directory, and are never executed here.
set -uo pipefail
HERE=$(dirname "$(readlink -f "$0")")
STUDY=""; IMAGE=""; GPU="RTX 3090"; MAX_DPH=0.30; MAX_HOURS=2; CAP=1; DISK=80; PUSH=""; JOB=""; OUT=""
LEDGER=${GPU_STUDY_ROOT}/ledger.csv; KEY=${GPU_STUDY_SECRETS}/rental_ed25519
SSH_WAIT=600; MAX_HOSTS=5; MAX_FILE_MB=4096; EXCLUDE=""; ONLY=""; MINRAM=23000
while [ $# -gt 0 ]; do case "$1" in
  --study) STUDY=$2; shift 2;; --image) IMAGE=$2; shift 2;; --gpu) GPU=$2; shift 2;;
  --max-dph) MAX_DPH=$2; shift 2;; --max-hours) MAX_HOURS=$2; shift 2;; --cap-dollars) CAP=$2; shift 2;;
  --disk) DISK=$2; shift 2;; --push-dir) PUSH=$2; shift 2;; --job) JOB=$2; shift 2;; --out) OUT=$2; shift 2;;
  --ledger) LEDGER=$2; shift 2;; --ssh-wait) SSH_WAIT=$2; shift 2;; --max-hosts) MAX_HOSTS=$2; shift 2;;
  --max-file-mb) MAX_FILE_MB=$2; shift 2;; --exclude-offers) EXCLUDE=$2; shift 2;; --prefer-machine) ONLY=$2; shift 2;; --min-gpu-ram-mb) MINRAM=$2; shift 2;;
  *) echo "unknown flag: $1" >&2; exit 2;; esac; done
for v in STUDY IMAGE JOB OUT; do [ -n "${!v}" ] || { echo "--${v,,} required" >&2; exit 2; }; done
[ -e "$OUT" ] && { echo "--out must not exist" >&2; exit 2; }
mkdir -p "$OUT"; KH=$(mktemp); ID=""; IP=""; PORT=""; DPH=""; GEO=""; OFFER=""; GOTNAME=""
V() { "$HERE/vast.sh" "$@"; }
log() { echo "$(date -Is) [$STUDY] $*"; }
ssh_i() { env -u SSH_AUTH_SOCK ssh -F /dev/null -n -p "$PORT" -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
  -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KH" -o ForwardAgent=no -o ForwardX11=no \
  -o PermitLocalCommand=no -o ConnectTimeout=15 -o ServerAliveInterval=30 -o ServerAliveCountMax=4 "root@$IP" "$@"; }
ssh_in() { env -u SSH_AUTH_SOCK ssh -F /dev/null -p "$PORT" -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
  -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KH" -o ForwardAgent=no -o ForwardX11=no \
  -o PermitLocalCommand=no -o ConnectTimeout=15 -o ServerAliveInterval=30 -o ServerAliveCountMax=4 "root@$IP" "$@"; }
C0=$(V --action credit); T0=$(date +%s); OUTCOME=unstarted; T_READY=""; T_JOB=""
led() { # one row per instance tried
  [ -f "$LEDGER" ] || echo "study,instance,offer,gpu_wanted,image,dph,geo,t_create,t_ready,t_job_end,t_destroy,credit_before,credit_after,outcome" >"$LEDGER"
  echo "$STUDY,$1,$2,$GPU,$IMAGE,$3,\"$4\",$5,$6,$7,$8,$9,${10},${11}" >>"$LEDGER"; }
destroy_one() { # ID
  local tries=0
  while [ $tries -lt 20 ]; do
    V --action destroy --instance "$1" 2>/dev/null
    sleep 5
    V --action list 2>/dev/null | jq -e --argjson i "$1" 'select(.id==$i)' >/dev/null || return 0
    tries=$((tries+1)); sleep 10
  done; log "DESTROY NOT CONFIRMED for $1"; return 1; }
cleanup() { trap - EXIT INT TERM
  if [ -n "$ID" ]; then destroy_one "$ID"; sleep 45; c=$(V --action credit)
    led "$ID" "$OFFER" "$DPH" "$GEO" "$T_CREATE" "$T_READY" "$T_JOB" "$(date +%s)" "$C0" "$c" "$OUTCOME"; log "destroyed $ID outcome=$OUTCOME credit $C0 -> $c"; fi
  rm -f "$KH"; }
trap cleanup EXIT; trap 'OUTCOME=interrupted; exit 3' INT TERM
BAD=${GPU_STUDY_ROOT}/bad-machines
[ -f "$BAD" ] && EXCLUDE="$EXCLUDE $(sed 's/^/m/' "$BAD" | tr '\n' ' ')"
tried=" $EXCLUDE "; hosts=0
while [ $hosts -lt "$MAX_HOSTS" ]; do
  offers=$(V --action search --gpu "$GPU" --min-gpu-ram-mb "$MINRAM" --max-dph "$MAX_DPH" --min-disk "$DISK" --limit 12 || true)
  # A machine that already pulled an image starts in a couple of minutes; try it first.
  offers=$( { echo "$offers" | jq -c --arg m "$ONLY" 'select((.machine_id|tostring)==$m)'; echo "$offers" | jq -c --arg m "$ONLY" 'select((.machine_id|tostring)!=$m)'; } )
  offer=$(echo "$offers" | jq -r '"\(.id) \(.machine_id)"' | while read -r o m; do case "$tried" in (*" m$m "*) ;; (*) echo "$o"; break;; esac; done)
  [ -n "$offer" ] || { log "no offers under $MAX_DPH"; OUTCOME=no-offers; exit 4; }
  hosts=$((hosts+1)); OFFER=$offer
  row=$(echo "$offers" | jq -c --argjson o "$offer" 'select(.id==$o)'); DPH=$(echo "$row"|jq -r .dph_total); GEO=$(echo "$row"|jq -r .geolocation)
  tried="$tried m$(echo "$row"|jq -r .machine_id) "
  T_CREATE=$(date +%s); DL=$(( T_CREATE + $(awk -v h="$MAX_HOURS" 'BEGIN{printf "%d", h*3600}') ))
  ID=$(V --action create --ask "$offer" --image "$IMAGE" --disk "$DISK" --label "usnm-gpu-$STUDY-d$DL") || { log "create failed on $offer"; ID=""; continue; }
  [ "$ID" != null ] && [ -n "$ID" ] || { ID=""; continue; }
  log "instance $ID on offer $offer dph=$DPH geo=$GEO"
  V --action attach-key --instance "$ID" --pubkey-file "$KEY.pub" || log "attach-key failed, retrying below"
  ok=0; end=$(( $(date +%s) + SSH_WAIT )); : >"$KH"
  while [ "$(date +%s)" -lt "$end" ]; do
    j=$(V --action list | jq -c --argjson i "$ID" 'select(.id==$i)')
    IP=$(echo "$j"|jq -r '.public_ipaddr//empty'|tr -d ' \n'); PORT=$(echo "$j"|jq -r '.ports["22/tcp"][0].HostPort//empty')
    if [ -n "$IP" ] && [ -n "$PORT" ]; then
      res=$(ssh_i 'echo ok' 2>&1)
      if echo "$res" | grep -qx ok; then ok=1; break; fi
      # Login reached the forced tmux. A terminal session gets past it, once, and turns it off for $HOME.
      if echo "$res" | grep -q 'no sessions'; then
        { sleep 4; printf 'touch "$HOME/.no_auto_tmux" /root/.no_auto_tmux; exit\n'; sleep 3; } |
          timeout 20 script -qfc "env -u SSH_AUTH_SOCK ssh -F /dev/null -tt -p $PORT -i $KEY -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=$KH root@$IP" /dev/null >/dev/null 2>&1
        ssh_i 'echo ok' 2>/dev/null | grep -qx ok && { ok=1; break; }
      fi
    fi
    V --action attach-key --instance "$ID" --pubkey-file "$KEY.pub" 2>/dev/null; sleep 20
  done
  if [ $ok = 1 ]; then T_READY=$(date +%s); log "ssh ready after $((T_READY-T_CREATE))s at $IP:$PORT"; break; fi
  log "no ssh in ${SSH_WAIT}s on $offer ($(V --action list | jq -r --argjson i "$ID" 'select(.id==$i)|"\(.actual_status): \(.status_msg)"' | tr -cd '[:print:]' | cut -c1-120)), moving on"; OUTCOME=no-ssh; cl=$ID; echo "$(echo "$row"|jq -r .machine_id)" >>"$BAD"
  destroy_one "$cl"; led "$cl" "$OFFER" "$DPH" "$GEO" "$T_CREATE" "" "" "$(date +%s)" "$C0" "$(V --action credit)" no-ssh; ID=""
done
[ -n "$ID" ] || { OUTCOME=no-host; exit 5; }
OUTCOME=push
ssh_i 'mkdir -p /work/in /work/out; which tar >/dev/null; nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader > /work/out/gpu.txt 2>&1; nproc >> /work/out/gpu.txt'
if [ -n "$PUSH" ]; then pushed=0; for t in 1 2 3 4; do
    if tar -C "$PUSH" -chf - . | ssh_in 'mkdir -p /work/in && tar -C /work/in -xf -'; then pushed=1; break; fi
    log "push try $t failed"; sleep 20; done
  [ $pushed = 1 ] || { OUTCOME=push-failed; exit 6; }; fi
ssh_in 'cat > /work/job.sh' <"$JOB" || { OUTCOME=push-failed; exit 6; }
log "pushed in $(( $(date +%s)-T_READY ))s"
ssh_i "setsid nohup bash -c 'timeout $(( DL-$(date +%s)-300 )) bash /work/job.sh > /work/out/job.log 2>&1; echo \$? > /work/job.exit' >/dev/null 2>&1 < /dev/null &"
# Copy finished files back; a file is kept only when its size matches the host's. Called during the job too,
# so a host that drops off doesn't take the work with it.
sync_out() {
  local list n s have ok=0
  list=$(ssh_i 'cd /work/out && find . -maxdepth 1 -type f -printf "%f %s\n"' 2>/dev/null) || return 1
  [ -n "$list" ] || return 1
  while read -r n s; do
    echo "$n" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._-]{0,120}$' || continue
    case "$s" in ''|*[!0-9]*) continue;; esac
    [ "$s" -le $(( MAX_FILE_MB*1024*1024 )) ] || { log "skip $n: $s bytes"; continue; }
    have=$(stat -c %s "$OUT/$n" 2>/dev/null || echo -1); [ "$have" = "$s" ] && continue
    ssh_i "cat /work/out/$n" 2>/dev/null | head -c "$s" >"$OUT/.$n.part"
    if [ "$(stat -c %s "$OUT/.$n.part")" = "$s" ]; then mv "$OUT/.$n.part" "$OUT/$n"; else rm -f "$OUT/.$n.part"; ok=1; fi
  done <<<"$list"
  return $ok
}
OUTCOME=running
SYNC_EVERY=${SYNC_EVERY:-10}
# Rent only for GPU work: sample the card each poll, confirm use within 10 minutes of the start, destroy after
# 15 idle minutes. The samples give the GPU hours against hours paid in gpu-hours.csv.
T_START=$(date +%s); LAST_BUSY=$T_START; POLLS=0; BUSY=0; CONFIRMED=0; IDLE_MIN=${IDLE_MIN:-15}; GPU_LOG=$OUT/gpu-samples.tsv
while :; do
  now=$(date +%s)
  [ $(( (POLLS+1) % SYNC_EVERY )) = 0 ] && sync_out
  g=$(ssh_i 'nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits' 2>/dev/null | tr -d ' ' | grep -E '^[0-9]+,[0-9]+$' | head -1)
  POLLS=$((POLLS+1)); u=${g%%,*}; m=${g##*,}; printf '%s\t%s\t%s\n' "$now" "${u:-}" "${m:-}" >>"$GPU_LOG"
  if [ -n "$u" ] && [ "$u" -ge 5 ] && [ "$m" -ge 500 ]; then BUSY=$((BUSY+1)); LAST_BUSY=$now
    [ $CONFIRMED = 1 ] || { CONFIRMED=1; log "GPU use confirmed ${u}% ${m}MiB after $((now-T_START))s"; }; fi
  [ $CONFIRMED = 0 ] && [ $((now-T_START)) -ge 600 ] && log "WARNING no GPU use 10 minutes after start"
  [ $((now-LAST_BUSY)) -ge $((IDLE_MIN*60)) ] && { log "GPU idle ${IDLE_MIN} minutes, stopping"; OUTCOME=gpu-idle; break; }
  e=$(ssh_i 'cat /work/job.exit 2>/dev/null'); [ -n "$e" ] && { log "job exit $e"; break; }
  [ "$now" -ge $((DL-240)) ] && { log "wall clock limit"; OUTCOME=wall-limit; break; }
  c=$(V --action credit); if awk -v a="$C0" -v b="$c" -v m="$CAP" 'BEGIN{exit !((a-b)>=m)}'; then log "instance spend cap hit ($C0 -> $c)"; OUTCOME=cap; break; fi
  log "$(ssh_i 'tail -n 1 /work/out/progress.txt 2>/dev/null' | tr -cd '[:print:]' | cut -c1-160) credit=$c"
  sleep 60
done
T_JOB=$(date +%s)
sync_out || { for t in 1 2 3 4 5 6 7 8 9 10; do log "fetch incomplete, retry $t"; sleep 60; sync_out && break; done; }
sync_out || { OUTCOME=fetch-incomplete; log "FETCH INCOMPLETE: files were lost with this instance"; }
[ "$OUTCOME" = running ] && OUTCOME="job-exit-$e"
[ -f "${LEDGER%/*}/gpu-hours.csv" ] || echo "study,instance,gpu,job_start,job_end,polls,busy_polls" >"${LEDGER%/*}/gpu-hours.csv"
echo "$STUDY,$ID,$GPU,$T_START,$T_JOB,$POLLS,$BUSY" >>"${LEDGER%/*}/gpu-hours.csv"
log "fetched $(ls "$OUT"|wc -l) files in $(( $(date +%s)-T_JOB ))s"
