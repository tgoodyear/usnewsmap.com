#!/usr/bin/env bash
# Thin Vast.ai API helper. Named flags only. The API key is read from --key-file and never printed.
# Actions: credit | list | search | create | attach-key | destroy
set -euo pipefail
ACTION=""; KEY_FILE="${VAST_KEY_FILE:-/run/usnm-gpu-studies/vast.key}"
GPU="RTX 3090"; MIN_DISK=80; MIN_RAM_MB=23000; LIMIT=10; MAX_DPH=0.40
ASK=""; IMAGE=""; DISK=80; LABEL=""; INSTANCE=""; PUBKEY=""
while [ $# -gt 0 ]; do case "$1" in
  --action) ACTION=$2; shift 2;; --key-file) KEY_FILE=$2; shift 2;;
  --gpu) GPU=$2; shift 2;; --min-disk) MIN_DISK=$2; shift 2;; --min-gpu-ram-mb) MIN_RAM_MB=$2; shift 2;;
  --limit) LIMIT=$2; shift 2;; --max-dph) MAX_DPH=$2; shift 2;;
  --ask) ASK=$2; shift 2;; --image) IMAGE=$2; shift 2;; --disk) DISK=$2; shift 2;;
  --label) LABEL=$2; shift 2;; --instance) INSTANCE=$2; shift 2;; --pubkey-file) PUBKEY=$2; shift 2;;
  *) echo "unknown flag: $1" >&2; exit 2;; esac; done
API=https://console.vast.ai/api
# The header goes to curl on a config file descriptor, so the key never appears in argv.
call() { # METHOD PATH [JSON]
  local m=$1 p=$2 d=${3:-}
  if [ -n "$d" ]; then
    curl -fsS --max-time 60 -K <(printf 'header = "Authorization: Bearer %s"\n' "$(cat "$KEY_FILE")") \
      -X "$m" -H 'Content-Type: application/json' -d "$d" "$API$p"
  else
    curl -fsS --max-time 60 -K <(printf 'header = "Authorization: Bearer %s"\n' "$(cat "$KEY_FILE")") -X "$m" "$API$p"
  fi
}
case "$ACTION" in
  credit) call GET /v0/users/current/ | jq -r .credit;;
  list) call GET /v1/instances/ | jq -c '.instances[]|{id,label,actual_status,status_msg,public_ipaddr,dph_total,start_date,gpu_name,ports,machine_id}';;
  search)
    q=$(jq -nc --arg g "$GPU" --argjson d "$MIN_DISK" --argjson r "$MIN_RAM_MB" --argjson l "$LIMIT" --argjson p "$MAX_DPH" '{
      verified:{eq:true}, rentable:{eq:true}, rented:{eq:false}, external:{eq:false},
      num_gpus:{eq:1}, gpu_name:{eq:$g}, gpu_ram:{gte:$r}, disk_space:{gte:$d},
      inet_down:{gte:300}, inet_up:{gte:100}, cpu_cores_effective:{gte:8}, reliability2:{gte:0.97},
      dph_total:{lte:$p}, inet_down_cost:{lte:0.005}, inet_up_cost:{lte:0.005}, cuda_max_good:{gte:12.4},
      geolocation:{notin:["CN","RU","BY","IR","KP"]}, direct_port_count:{gte:1},
      type:"on-demand", order:[["dph_total","asc"]], limit:$l}')
    call POST /v0/bundles/ "$q" | jq -c '.offers[]|{id,machine_id,gpu_name,dph_total,geolocation,reliability2,inet_down,inet_up,inet_down_cost,inet_up_cost,cpu_cores_effective,disk_space,storage_cost,cuda_max_good}';;
  create)
    b=$(jq -nc --arg i "$IMAGE" --arg l "$LABEL" --argjson d "$DISK" '{
      client_id:"me", image:$i, disk:$d, label:$l, runtype:"ssh_direc ssh_proxy",
      env:"-e HOME=/root", onstart:"touch /root/.no_auto_tmux /tmp/.no_auto_tmux"}')
    call PUT "/v0/asks/$ASK/" "$b" | jq -r '.new_contract';;
  attach-key) call POST "/v0/instances/$INSTANCE/ssh/" "$(jq -nc --rawfile k "$PUBKEY" '{ssh_key:$k}')" >/dev/null;;
  destroy) call DELETE "/v0/instances/$INSTANCE/" >/dev/null;;
  *) echo "--action credit|list|search|create|attach-key|destroy" >&2; exit 2;;
esac
