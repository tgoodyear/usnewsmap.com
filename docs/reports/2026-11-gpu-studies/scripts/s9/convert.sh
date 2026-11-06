#!/usr/bin/env bash
set -eu
PATCH="$(cd "$(dirname "$0")" && pwd)/converter.patch"
[ -f "$PATCH" ] || { echo "missing $PATCH" >&2; exit 6; }
cd /root/work/s9-gguf
UV=/root/work/gpu-studies/local/uvbin/uv
[ -d llama.cpp ] || { git clone -q --filter=blob:none --no-checkout https://github.com/ggml-org/llama.cpp.git && cd llama.cpp && git checkout -q e1a9a6dcb && cd ..; }
git -C llama.cpp rev-parse HEAD > commit.txt
# The tokenizer-hash mapping is required: apply it, or accept it only if it is already applied.
if git -C llama.cpp apply --check "$PATCH" 2>/dev/null; then
  git -C llama.cpp apply "$PATCH"
elif ! git -C llama.cpp apply --reverse --check "$PATCH" 2>/dev/null; then
  echo "converter.patch neither applies nor is already applied" >&2; exit 6
fi
$UV venv -q --python 3.12 venv
. venv/bin/activate
$UV pip install -q "torch~=2.6.0" --index-url https://download.pytorch.org/whl/cpu > pip.log 2>&1
$UV pip install -q "numpy~=1.26.4" "protobuf>=4.21.0,<5.0.0" "sentencepiece>=0.1.98,<0.3.0" "transformers==5.5.1" ./llama.cpp/gguf-py >> pip.log 2>&1
python llama.cpp/convert_hf_to_gguf.py model --outtype q8_0 --outfile xiaomi-ocr-0-q8_0.gguf > convert-text.log 2>&1
python llama.cpp/convert_hf_to_gguf.py model --mmproj --outtype f16 --outfile mmproj-xiaomi-ocr-0-f16.gguf > convert-mmproj.log 2>&1
sha256sum *.gguf > gguf.sha256
echo "done" > DONE
