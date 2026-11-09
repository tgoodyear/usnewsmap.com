#!/usr/bin/env bash
# Round two of the Phoenix fine-tune (#1621). Runs in the usnm-chr-bank image with /w = /scratch/usnm-chr.
# Real lines keep the numerals and punctuation LoC read between trusted words; synthetic lines get
# verse numbers, punctuation and margin slivers. Training continues from round one's model
# (ft-v2-base/chr.traineddata = ft-v1/chr_phoenix.traineddata) for 20,000 more iterations.
set -euo pipefail
cd /w/bank
python3 tools/chr_real_lines.py --alto-dir loc --images images --vocab chr-attested.txt \
  --exclude-manifest /w/testset/v1/manifest.json --out real-v2
python3 tools/chr_synth_lines.py --bank bank-rs --text /w/testset/v2/cnts.raw.txt \
  --exclude-manifest /w/testset/v1/manifest.json --out synth-v2 --lines 20000 --seed 1622 \
  --marks-share 0.4 --punct-rate 0.15 --quote-rate 0.08 --edge-rate 0.3
bash tools/chr_finetune.sh --base ft-v2-base --real real-v2 --synth synth-v2 --work ft-v2 --iterations 20000 --jobs 6
