#!/usr/bin/env bash
# Study 5 step 1: the 2B baseline and the Study 2 adapter on the confirmation sets, unguarded. The guard is applied to these on our side.
set -u
O=/work/out; I=/work/in; PX="--min-pixels 1048576 --max-pixels 2097152"
python $I/lora_infer.py --list $I/new.jsonl --root $I --out $O/new-baseline.jsonl $PX
python $I/lora_infer.py --list $I/new.jsonl --root $I --adapter $I/adapter-r8-s1000.safetensors --rank 8 --out $O/new-adapter.jsonl $PX
