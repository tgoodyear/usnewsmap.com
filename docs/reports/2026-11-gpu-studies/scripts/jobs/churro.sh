#!/usr/bin/env bash
# CHURRO alone, on Python 3.12 as its package requires, with torch for CUDA 12.8, at the commit and revision of the 2026-10-18 trial.
set -u
O=/work/out; I=/work/in
unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE HF_DATASETS_OFFLINE
export HF_HOME=/work/hf TOKENIZERS_PARALLELISM=false UV_CACHE_DIR=/work/uvcache UV_PYTHON_INSTALL_DIR=/work/uvpy HOME=/root
# uv 0.13.6, the version uv.log records. The installer carries the sha256 of the release tarball it fetches.
UV_INSTALLER_SHA256=f7a56e7afaec49a3bcbc3ac3cdbe917e7c6ce30e0d815308a12e531aa5ef23b7
curl -fsSL -o /work/uv-install.sh https://astral.sh/uv/0.13.6/install.sh
echo "$UV_INSTALLER_SHA256  /work/uv-install.sh" | sha256sum -c --quiet - || { echo "uv installer does not match the pinned sha256" >> $O/progress.txt; exit 6; }
env UV_INSTALL_DIR=/work/uvbin UV_NO_MODIFY_PATH=1 sh /work/uv-install.sh > $O/uv.log 2>&1
export PATH=/work/uvbin:$PATH
uv venv -q --python 3.12 /work/cv >> $O/uv.log 2>&1
uv pip install -q --python /work/cv/bin/python 'torch==2.8.0' 'torchvision==0.23.0' --index-url https://download.pytorch.org/whl/cu128 >> $O/uv.log 2>&1
uv pip install -q --python /work/cv/bin/python 'churro-ocr[hf] @ git+https://github.com/stanford-oval/Churro.git@23c3ac65781443b18d87c8cdac5f77445c3319e6' accelerate >> $O/uv.log 2>&1
uv pip freeze --python /work/cv/bin/python > $O/pip-freeze.txt 2>&1
/work/cv/bin/python -c 'import churro_ocr.ocr, torchvision; from churro_ocr.providers.hf import _load_hf_runtime; _load_hf_runtime()' > $O/churro-import.log 2>&1 || { echo "import failed" >> $O/progress.txt; exit 7; }
/work/cv/bin/python $I/hf_engines.py --engine churro --revision ca2150ea465d5a3d67818c50e234b9422619c75d --list $I/crops.jsonl --root $I --out $O/churro.jsonl > $O/churro.log 2>&1
echo "done churro exit $?" >> $O/progress.txt
