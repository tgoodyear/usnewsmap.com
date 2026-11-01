#!/usr/bin/env bash
# Granite-Docling on the local card without containers, in a tmpfs venv with the training image's pinned versions.
# An image pull there holds podman's store lock against the BOINC client, so nothing is pulled. Same settings as the rented part.
set -u
D=/tmp/gpu-studies/s3granite; O=$D/out; I=$D/in; mkdir -p $O
export HF_HOME=$D/hf UV_CACHE_DIR=$D/uvcache UV_PYTHON_INSTALL_DIR=$D/uvpy TOKENIZERS_PARALLELISM=false HOME=$D
curl -fsSL https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=$D/uvbin UV_NO_MODIFY_PATH=1 sh > $O/uv.log 2>&1
export PATH=$D/uvbin:$PATH
uv venv -q --python 3.12 $D/v >> $O/uv.log 2>&1
uv pip install -q --python $D/v/bin/python 'torch==2.8.0' --index-url https://download.pytorch.org/whl/cu128 >> $O/uv.log 2>&1
uv pip install -q --python $D/v/bin/python 'transformers==5.20.0' 'accelerate==1.15.0' 'pillow==13.0.0' 'safetensors==0.8.0' >> $O/uv.log 2>&1
sed -i 's#/work/out/progress.txt#'"$O"'/progress.txt#' $I/hf_engines.py
s=$(date +%s); nice -n 5 $D/v/bin/python $I/hf_engines.py --engine granite --revision 982fe3b40f2fa73c365bdb1bcacf6c81b7184bfe --max-new-tokens 1024 --list $I/latin-crops.jsonl --root $I --out $O/granite.jsonl > $O/granite.log 2>&1
echo "granite exit $? seconds $(( $(date +%s)-s ))" >> $O/progress.txt
