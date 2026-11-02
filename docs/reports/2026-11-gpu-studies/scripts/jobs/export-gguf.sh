#!/usr/bin/env bash
# Study 5 step 3: merge, convert and quantize with the llama.cpp commit of the pinned server image. Nothing is signed or published.
set -u
O=/work/out; I=/work/in; . $I/export.env   # NAME ADAPTER QUANT BASEREPO BASEREV (empty BASEREPO means the image's 2B)
LLAMA=091b6a7fc
apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq cmake g++ git >/dev/null 2>&1
git clone -q https://github.com/ggml-org/llama.cpp /work/llama 2>> $O/export.log; git -C /work/llama checkout -q $LLAMA 2>> $O/export.log
git -C /work/llama log -1 --format='%H %s' > $O/llama-commit.txt
PY=$(command -v python); $PY -m pip -q install --no-cache-dir -e /work/llama/gguf-py sentencepiece protobuf mistral-common 2>&1 | tail -1 >> $O/export.log
cmake -S /work/llama -B /work/lb -DGGML_NATIVE=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=OFF -DLLAMA_CURL=OFF >> $O/export.log 2>&1
cmake --build /work/lb --target llama-quantize -j"$(nproc)" >> $O/export.log 2>&1
BASE=/opt/base
if [ -n "${BASEREPO:-}" ]; then
  unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE; $PY -c "from huggingface_hub import snapshot_download as d; d('$BASEREPO', revision='$BASEREV', local_dir='/work/base', allow_patterns=['*.json','*.safetensors','*.txt','*.jinja'])" >> $O/export.log 2>&1; BASE=/work/base
  export HF_HUB_OFFLINE=1
fi
$PY $I/merge_lora.py --base $BASE --adapter $I/$ADAPTER --out /work/merged >> $O/export.log 2>&1 || exit 5
$PY /work/llama/convert_hf_to_gguf.py /work/merged --outtype f16 --outfile /work/$NAME-f16.gguf >> $O/export.log 2>&1 || exit 6
/work/lb/bin/llama-quantize /work/$NAME-f16.gguf $O/$NAME-$QUANT.gguf $QUANT >> $O/export.log 2>&1 || exit 7
sha256sum $O/$NAME-$QUANT.gguf > $O/$NAME.sha256
echo done >> $O/progress.txt
