#!/bin/bash
# Smoke run of the combined segmenter pipeline (#2107) on a small sample of TRAINING pages only:
# 70 La Liberte pages from the clean training pool (one parquet shard) and 30 spent-test Chronicling Germany
# non-Test pages, as written by train_inventory.py --sample-n/--sample-cg-n. Checks that pooling, features,
# labels, training and inference run end to end; nothing here is a result.
#   ROOT=/srv/comboseg AS_SHA=... TR_SHA=... bash smoke.sh
set -euo pipefail
ROOT=${ROOT:-/srv/comboseg}
PY="$ROOT/venv/bin/python"
C="$ROOT/code"
IN="$ROOT/in"
W="$ROOT/work/smoke"
AS_SHA=${AS_SHA:-045b2e5588e53c700490730244bdc3e8ff21e903aff1c2af9b169dcdb1d9155e}
TR_SHA=${TR_SHA:-7d75048e1192c22e501ffc1f649d74d80fa4218c824455fbc699a66e5005b62f}
mkdir -p "$W"
export HOME="$ROOT" OMP_NUM_THREADS=2 YOLO_CONFIG_DIR="$ROOT/.ultralytics"
cd "$W"

# references and image paths for the sample (La Liberte rows point at /data/layout-refs; here it is $IN)
"$PY" - "$ROOT" <<'EOF'
import json, os, sys
root = sys.argv[1]
sys.path.insert(0, os.path.join(root, "code", "layout_diag"))
import cg_refs
inv = os.path.join(root, "work", "inventory")
w = os.path.join(root, "work", "smoke")
p = json.load(open(os.path.join(inv, "smoke-liberte-paths.json")))
json.dump({k: v.replace("/data/layout-refs/", os.path.join(root, "in") + "/") for k, v in p.items()},
          open(os.path.join(w, "liberte-paths.json"), "w"), sort_keys=True)
os.system("cp %s %s" % (os.path.join(inv, "smoke-liberte-refs.json"), os.path.join(w, "liberte-refs.json")))
d = os.path.join(root, "in", "data", "chronicling-germany", "66b50f53", "data")
refs, paths = {}, {}
for pid in [l.strip() for l in open(os.path.join(inv, "smoke-cg-ids.txt")) if l.strip()]:
    regs, wd, ht = cg_refs.parse_page(os.path.join(d, "annotations", pid + ".xml"))
    refs[pid] = cg_refs.reference(regs, wd, ht)
    paths[pid] = os.path.join(d, "images", pid + ".jpg")
json.dump({"for_publication": False, "pages": refs, "set": "cg-smoke"}, open(os.path.join(w, "cg-refs.json"), "w"), sort_keys=True)
json.dump(paths, open(os.path.join(w, "cg-paths.json"), "w"), sort_keys=True)
print("refs", len(p), len(refs))
EOF

for S in liberte cg; do
  "$PY" "$C/layout_columns/pagecache.py" --paths "$W/$S-paths.json" --out-dir "$W/cache" --workers 2
  "$PY" "$C/combo_seg/detect.py" --page-cache "$W/cache" --pages-from "$W/$S-refs.json" --out-dir "$W/det-$S" \
      --as-model "$IN/models/NealCaren__american-stories-onnx/layout_model_new.onnx" --as-sha256 "$AS_SHA" \
      --trained-model "$IN/models/usnm-layout-2107/best.pt" --trained-sha256 "$TR_SHA" --threads 3
done
"$PY" "$C/combo_seg/wordboxes.py" --page-cache "$W/cache" --pages-from "$W/liberte-refs.json" --out-dir "$W/words" --lang fra --workers 3
"$PY" "$C/combo_seg/wordboxes.py" --page-cache "$W/cache" --pages-from "$W/cg-refs.json" --out-dir "$W/words" --lang deu --workers 3
for S in liberte cg; do
  "$PY" "$C/combo_seg/build_features.py" --page-cache "$W/cache" --detections-dir "$W/det-$S" --refs "$W/$S-refs.json" \
      --paths "$W/$S-paths.json" --words-dir "$W/words" --set-name "$S" --out "$W/feats-$S.jsonl" --workers 3
done
"$PY" "$C/combo_seg/train.py" --features "$W/feats-liberte.jsonl" "$W/feats-cg.jsonl" --refs "$W/liberte-refs.json" "$W/cg-refs.json" \
    --out-model "$W/model.pkl" --report "$W/train-report.json" --dev-frac 0.2 \
    --baseline-dev-precision "${BASELINE_DEV_P:-0}"   # smoke only: a real run passes the baseline's dev precision
"$PY" "$C/combo_seg/infer.py" --features "$W/feats-liberte.jsonl" --model "$W/model.pkl" --pages-from "$W/liberte-refs.json" \
    --out "$W/detections-liberte.json"
"$PY" - "$W" <<'EOF'
import json, os, sys
w = sys.argv[1]
spec = {"liberte-smoke": {"refs": os.path.join(w, "liberte-refs.json"), "detections": [os.path.join(w, "detections-liberte.json")]}}
json.dump(spec, open(os.path.join(w, "sets.json"), "w"))
EOF
"$PY" "$C/layout_columns/score.py" --sets-json "$W/sets.json" --out "$W/score-check.json" --no-per-page > "$W/score-check.log"
echo "score.py read the inference output (scorability check only; training pages, in-sample)"
