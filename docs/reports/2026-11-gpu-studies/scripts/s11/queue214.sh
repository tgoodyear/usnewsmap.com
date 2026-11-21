#!/usr/bin/env bash
# Study 11 GPU queue on CT 214, one reader at a time on the 2060 SUPER: wait for the Xiaomi region run, then Xiaomi on
# the page arm's line crops, then Qwen 8B request version 3 on the regions (ctx 4096, then a second pass at ctx 8192 for
# any crop the first pass could not read). No flags.
set -uo pipefail
L=/root/s11/queue.log; log() { echo "$(date -Is) $*" >>"$L"; }
idle() { for _ in $(seq 1 60); do [ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')" -le 300 ] && return 0; sleep 5; done; return 1; }
while systemctl is-active -q s11-xiaomi; do sleep 30; done
log "xiaomi regions finished"
idle || { log "GPU not idle"; exit 3; }
/root/s9/deployed3.sh --llama /root/s9/dl-b12084-vulkan-x64/llama-b12084 --gguf /root/s9/gguf --in /root/s11/lines --arm gpu --out /root/s11/xiaomi-lines --python /root/s9/venv/bin/python
log "xiaomi lines exit $?"
idle || { log "GPU not idle"; exit 3; }
B=/var/lib/boinc-client/projects/boinc.usnewsmap.com
Q=(--llama /root/s9/dl-b12084-vulkan-x64/llama-b12084 --model $B/usnm_qwen_67d1659bfe71b89d50b45a4ad1a9e5b997e5bb16ce5da66a6a6167abd569e9e2_Qwen3VL-8B-Instruct-Q4_K_M.gguf
   --mmproj $B/usnm_qwen_c6ba85508d82f42590e6eb77d5340369ab6fecf107a7561d809523d8aa5f3bfd_mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf
   --in /root/s11/in --list /root/s11/in/crops.jsonl --out /root/s11/qwen --python /root/s9/venv/bin/python --script /root/s11/scripts/qwen_read.py --max-tokens 2048)
/root/s11/scripts/qwen-serve.sh "${Q[@]}" --ctx 4096
log "qwen ctx 4096 exit $?"
idle || { log "GPU not idle"; exit 3; }
/root/s11/scripts/qwen-serve.sh "${Q[@]}" --ctx 8192
log "qwen ctx 8192 exit $?"
log "queue done"
