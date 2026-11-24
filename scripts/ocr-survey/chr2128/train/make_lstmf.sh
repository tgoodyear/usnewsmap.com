#!/usr/bin/env bash
# One .lstmf per training line, as round two's chr_finetune.sh step 1 makes them (#2128).
#   make_lstmf.sh --src DIR --out DIR [--jobs N]
# DIR holds <name>.png + <name>.gt.txt; each becomes <out>/<basename of DIR>__<name>.lstmf.
set -euo pipefail
JOBS=4
while [ $# -gt 0 ]; do case "$1" in
  --src) SRC=$2; shift 2;; --out) OUT=$2; shift 2;; --jobs) JOBS=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
mkdir -p "$OUT"
make_one() {
  png=$1; base=${png%.png}; name=$(basename "$base"); dir=$(dirname "$png")
  out="$OUT/$(basename "$dir")__$name"
  [ -s "$out.lstmf" ] && return 0
  read -r w h < <(python3 -c "from PIL import Image;im=Image.open('$png');print(*im.size)")
  text=$(head -1 "$base.gt.txt")
  printf 'WordStr 0 0 %d %d 0 #%s\n\t %d %d %d %d 0\n' "$w" "$h" "$text" "$w" "$h" "$((w+1))" "$((h+1))" > "$out.box"
  cp "$png" "$out.png"
  OMP_THREAD_LIMIT=1 tesseract "$out.png" "$out" --psm 13 lstm.train >/dev/null 2>&1 || echo "failed $png" >&2
  rm -f "$out.png"
}
export -f make_one; export OUT
find "$SRC" -maxdepth 1 -name '*.png' | sort | xargs -P "$JOBS" -I{} bash -c 'make_one {}'
echo "$(find "$OUT" -name "$(basename "$SRC")__*.lstmf" | wc -l) lstmf from $SRC"
