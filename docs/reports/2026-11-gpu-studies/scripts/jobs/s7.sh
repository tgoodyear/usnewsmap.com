#!/usr/bin/env bash
# Study 7: image-token sweep on one Qwen 8B GGUF. /work/in/list.tsv: id, relative image path, max_tokens.
# Only files under /work/in are read; Linux /proc and /dev/fd are required to verify open descriptors.
set -u
O=/work/out; I=/work/in; M=/opt/qwen/models; PORT=8090
MODEL=$M/Qwen3VL-8B-Instruct-Q4_K_M.gguf; MM=$M/mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf
PROMPT='Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, line breaks and hyphenation. Do not correct or modernise anything, and do not add anything that is not printed. If a word is unreadable, write [?] for it. Output only the transcription.'
[ -d "$I" ] && [ ! -L "$I" ] || { echo "invalid Study 7 input root" >&2; exit 6; }
PARENT=$(realpath -- "${I%/*}") || { echo "invalid Study 7 input root" >&2; exit 6; }
ROOT=$(realpath -- "$I") || { echo "invalid Study 7 input root" >&2; exit 6; }
[ "$ROOT" = "${PARENT%/}/${I##*/}" ] || { echo "invalid Study 7 input root" >&2; exit 6; }
input_path() {
  case "$1" in ''|/*|..|../*|*/..|*/../*) return 1;; esac
  local p; p=$(realpath -- "$ROOT/$1" 2>/dev/null) || return 1
  case "$p" in
    "$ROOT"/*) [ -f "$p" ] && [ -r "$p" ] || return 1; printf '%s\n' "$p";;
    *) return 1;;
  esac
}
input_fd_ok() {
  local p
  [ -f "/dev/fd/$1" ] || return 1
  # Verify the opened file too, so a symlink swap cannot redirect a later read.
  p=$(realpath -- "/proc/self/fd/$1" 2>/dev/null) || return 1
  case "$p" in "$ROOT"/*) return 0;; *) return 1;; esac
}
parse_row() {
  local row=$1 rest
  case "$row" in *$'\t'*$'\t'*) ;; *) return 1;; esac
  id=${row%%$'\t'*}; rest=${row#*$'\t'}; rel=${rest%%$'\t'*}; mt=${rest#*$'\t'}
  case "$mt" in *$'\t'*) return 1;; esac
  [ -n "$id" ] && [ -n "$rel" ] && [[ $mt =~ ^-?(0|[1-9][0-9]*)$ ]]
}
LIST=$(input_path list.tsv) || { echo "invalid Study 7 list.tsv" >&2; exit 6; }
exec 4< "$LIST" || { echo "invalid Study 7 list.tsv" >&2; exit 6; }
input_fd_ok 4 || { echo "invalid Study 7 list.tsv" >&2; exit 6; }
LIST_DATA=""
if IFS= read -r -d '' LIST_DATA <&4; then echo "invalid NUL in Study 7 list.tsv" >&2; exit 6; fi
exec 4<&-
rows=(); count=0
if [ -n "$LIST_DATA" ]; then
  LIST_DATA=${LIST_DATA%$'\n'}
  while IFS= read -r row; do
    parse_row "$row" || { echo "invalid Study 7 list.tsv row" >&2; exit 6; }
    input_path "$rel" >/dev/null || { echo "invalid or escaping Study 7 image path" >&2; exit 6; }
    rows[count]=$row; count=$((count+1))
  done <<<"$LIST_DATA"
fi
unset LIST_DATA
# Native Vulkan build if it finds the card, else the image's CUDA build.
SRV=/app/llama-server; export LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:/app; RT=cuda
if [ -x $I/rt/llama-server ]; then
  apt-get install -y -qq libvulkan1 >/dev/null 2>&1
  mkdir -p /etc/vulkan/icd.d; [ -f /etc/vulkan/icd.d/nvidia_icd.json ] || printf '{"file_format_version":"1.0.0","ICD":{"library_path":"libGLX_nvidia.so.0","api_version":"1.3"}}\n' >/etc/vulkan/icd.d/nvidia_icd.json
  if LD_LIBRARY_PATH=$I/rt:/usr/lib/x86_64-linux-gnu $I/rt/llama-server --list-devices 2>&1 | tee $O/vulkan-devices.txt | grep -q Vulkan0; then SRV=$I/rt/llama-server; export LD_LIBRARY_PATH=$I/rt:/usr/lib/x86_64-linux-gnu; RT=vulkan; fi
fi
echo "$RT" > $O/runtime.txt; $SRV --version > $O/llama.version 2>&1; [ $RT = cuda ] && $SRV --list-devices 2>&1 | grep -q CUDA0 || [ $RT = vulkan ] || { echo "no GPU device" >> $O/progress.txt; exit 8; }
cd "$(dirname "$SRV")" || exit 6
stop_server() { kill "$LP" 2>/dev/null || true; }
abort_run() { echo "$1" >&2; stop_server; exit 6; }
for v in default 768 1024 1536 2048 3072; do
  CTX=4096; set --; if [ "$v" != default ]; then set -- --image-min-tokens "$v"; [ $((v+2304)) -gt 4096 ] && CTX=$((v+2304)); fi
  $SRV -m $MODEL --mmproj $MM --host 127.0.0.1 --port $PORT -ngl 99 --threads 8 --ctx-size $CTX --temp 0 --seed 42 --parallel 1 --no-webui "$@" > $O/llama-$v.log 2>&1 &
  LP=$!; for _ in $(seq 240); do curl -fsS -m 2 http://127.0.0.1:$PORT/health >/dev/null 2>&1 && break; kill -0 $LP 2>/dev/null || break; sleep 2; done
  kill -0 $LP 2>/dev/null || { echo "$v server failed" >> $O/progress.txt; break; }
  T1=$(date +%s); n=0; fail=0; peak=0
  for ((idx=0; idx<count; idx++)); do
    IFS=$'\t' read -r id rel mt <<<"${rows[$idx]}"
    f=$(input_path "$rel") || abort_run "invalid or escaping Study 7 image path"
    exec 3< "$f" || abort_run "failed to open Study 7 image"
    input_fd_ok 3 || abort_run "invalid or escaping Study 7 image path"
    case "$rel" in *.png) ty=png;; *) ty=jpeg;; esac
    if ! { printf '{"max_tokens":%s,"temperature":0,"seed":42,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/%s;base64,' "$mt" "$ty" &&
      base64 -w0 <&3 && printf '"}},{"type":"text","text":"%s"}]}]}' "$PROMPT"; } > /tmp/b.json; then
      abort_run "failed to read Study 7 image"
    fi
    exec 3<&-
    s=$(date +%s%N); curl -sS -m 900 -H 'Content-Type: application/json' --data-binary @/tmp/b.json http://127.0.0.1:$PORT/v1/chat/completions > /tmp/r.json 2>/dev/null; e=$(date +%s%N)
    printf '%s\t%s\t%s\n' "$id" "$(( (e-s)/1000000 ))" "$(base64 -w0 /tmp/r.json)" >> $O/s7-$v.b64
    u=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' '); [ "${u:-0}" -gt "$peak" ] && peak=$u
    n=$((n+1)); [ $((n%10)) = 0 ] && echo "$v $n $(( $(date +%s)-T1 ))s" >> $O/progress.txt
    grep -q 'out of memory\|ErrorOutOfDeviceMemory\|failed to allocate' $O/llama-$v.log && { fail=1; break; }
    kill -0 $LP 2>/dev/null || { fail=1; break; }
  done
  echo "$v seconds $(( $(date +%s)-T1 )) peak_mib $peak fail $fail" >> $O/timing.txt
  stop_server; sleep 3; [ $fail = 1 ] && { echo "$v stopped the sweep" >> $O/progress.txt; break; }
done
echo "done" >> $O/progress.txt
