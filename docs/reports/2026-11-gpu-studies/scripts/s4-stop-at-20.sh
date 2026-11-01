#!/usr/bin/env bash
# Stop each Study 4 Surya job once both hold the same 20 pages: the rate is stable and the rest would only cost money.
cd ${GPU_STUDY_ROOT}
cnt() { j=$(scripts/vast.sh --action list | grep "$1"); [ -n "$j" ] || { echo -1; return; }; IP=$(echo "$j"|jq -r '.public_ipaddr'|tr -d ' \n'); PORT=$(echo "$j"|jq -r '.ports["22/tcp"][0].HostPort//empty'); env -u SSH_AUTH_SOCK ssh -F /dev/null -n -p $PORT -i ${GPU_STUDY_SECRETS}/rental_ed25519 -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/tmp/kh-w$1 -o ConnectTimeout=15 root@$IP "$2" 2>/dev/null | tail -1; }
while :; do
  for k in s4surya3090 s4surya4090; do n=$(cnt $k 'wc -l < /work/out/surya-pages.jsonl'); echo "$(date -Is) $k $n" >> s4watch.log
    if [ "${n:-0}" -ge 20 ]; then cnt $k 'pkill -f surya_read.py'; fi; done
  scripts/vast.sh --action list | grep -q s4surya || break
  sleep 120
done
echo S4-WATCH-DONE >> s4watch.log
