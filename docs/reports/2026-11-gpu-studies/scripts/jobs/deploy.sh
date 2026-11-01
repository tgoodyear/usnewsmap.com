#!/usr/bin/env bash
# Study 5 step 3: the stock and merged GGUFs on one crop list, in the usnm_qwen image, request version 2. models.tsv: name<TAB>model<TAB>mmproj.
set -u
I=/work/in
while IFS=$'\t' read -r name model mm; do
  { printf 'MODEL=%s\nMMPROJ=%s\nMAX_TOKENS=2048\nMIN_TOKENS=1024\nREADOUT=%s.b64\n' "$model" "$mm" "$name"; } > $I/params.env
  bash $I/qwen.sh
done < $I/models.tsv
