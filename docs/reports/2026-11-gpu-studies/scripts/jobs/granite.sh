#!/usr/bin/env bash
# Granite-Docling alone, with a 1,024-token cap. At 4,096 tokens it took about 97 s a crop, running to the cap.
set -u
O=/work/out; I=/work/in
unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE HF_DATASETS_OFFLINE
export HF_HOME=/work/hf TOKENIZERS_PARALLELISM=false
$(command -v python) $I/hf_engines.py --engine granite --revision 982fe3b40f2fa73c365bdb1bcacf6c81b7184bfe --max-new-tokens 1024 --list $I/latin-crops.jsonl --root $I --out $O/granite.jsonl > $O/granite.log 2>&1
echo "done granite exit $?" >> $O/progress.txt
