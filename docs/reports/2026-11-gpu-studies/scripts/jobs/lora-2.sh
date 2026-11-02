#!/usr/bin/env bash
# Phase 2: read the test split once, baseline and the one selected adapter. /work/in/selected.txt names the adapter.
set -u
O=/work/out; I=/work/in; PX="--min-pixels 1048576 --max-pixels 2097152"
sel=$(cat $I/selected.txt); sha256sum "$I/$sel" > $O/adapter.sha256
python $I/lora_infer.py --list $I/test.jsonl --root $I --out $O/test-baseline.jsonl $PX
python $I/lora_infer.py --list $I/test.jsonl --root $I --adapter "$I/$sel" --rank 8 --out $O/test-adapter.jsonl $PX
