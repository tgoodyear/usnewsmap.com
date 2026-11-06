#!/usr/bin/env bash
# Study 5 step 2, phase 2: test once with the guard. Base and selected adapter on the second-use and confirmation sets.
set -u
O=/work/out; I=/work/in; PX=(--min-pixels 1048576 --max-pixels 2097152)
G=$(cat "$I/guard.txt"); REV=$(cat "$I/base-revision.txt") || exit 6; SEL=$(cat "$I/selected.txt")
if ! [[ $REV =~ ^[0-9a-f]{40}$ ]] ||
  ! { printf '%s\n' "$REV" | cmp -s "$I/base-revision.txt" - || printf '%s' "$REV" | cmp -s "$I/base-revision.txt" -; }; then
  echo "invalid base revision" >&2; exit 6
fi
unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE HF_DATASETS_OFFLINE; export HF_HOME=/work/hf
PY=$(command -v python)
"$PY" -c 'import sys; from huggingface_hub import snapshot_download as d; d("Qwen/Qwen3-VL-8B-Instruct", revision=sys.argv[1], local_dir="/work/base8", allow_patterns=["*.json","*.safetensors","*.txt","*.jinja"])' "$REV" > "$O/download.log" 2>&1 || exit 6
sha256sum "$I/$SEL" > "$O/adapter.sha256"
[ "$(cat "$I/part.txt" 2>/dev/null)" = adapter ] || "$PY" "$I/lora_infer.py" --base /work/base8 --guard "$G" --list "$I/all.jsonl" --root "$I" --out "$O/test-baseline.jsonl" "${PX[@]}"
[ "$(cat "$I/part.txt" 2>/dev/null)" = baseline ] || "$PY" "$I/lora_infer.py" --base /work/base8 --guard "$G" --list "$I/all.jsonl" --root "$I" --adapter "$I/$SEL" --rank 8 --out "$O/test-adapter.jsonl" "${PX[@]}"
