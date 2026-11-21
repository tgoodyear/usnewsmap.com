#!/usr/bin/env bash
# The production damage rate and known-word count (usnm_core::damage::score_forced) for every reading,
# with the gain-proxy report's proxy-rows tool (docs/reports/2026-11-gain-proxy-validation/scripts/proxy-rows,
# PR #1889).
#   damage.sh --rows rows/truth.jsonl --proxy-rows PATH --out damage-truth.jsonl
set -euo pipefail
while [ $# -gt 0 ]; do case "$1" in --rows) ROWS="$2"; shift 2;; --proxy-rows) BIN="$2"; shift 2;; --out) OUT="$2"; shift 2;; *) exit 2;; esac; done
python3 -c '
import json, sys
for l in open(sys.argv[1], encoding="utf-8"):
    r = json.loads(l)
    print(json.dumps({"id": r["reader"] + "|" + r["unit"], "lang": r["lang"], "ref": r.get("ref", ""), "hyp": r["hyp"]}, ensure_ascii=False))
' "$ROWS" | nice "$BIN" > "$OUT"
