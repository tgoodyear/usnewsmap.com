#!/usr/bin/env bash
# Study 5 step 1: read the validation split under each guard candidate. Unguarded readings are Study 2's.
set -u
O=/work/out; I=/work/in; PX="--min-pixels 1048576 --max-pixels 2097152"
for g in stop rp105 rp110 stop-retry; do
  python $I/lora_infer.py --list $I/validation.jsonl --root $I --guard $g --out $O/val-$g.jsonl $PX
done
echo done >> $O/progress.txt
