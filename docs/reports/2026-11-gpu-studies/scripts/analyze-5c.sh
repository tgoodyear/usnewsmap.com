#!/usr/bin/env bash
# Study 5: confirmation sets and the deployed path for 2B. Every guarded column is the stop guard applied to finished readings.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s5; mkdir -p "$A"
{ cat lora/test-refs.jsonl; jq -c 'del(.file)' s5sets/bln.jsonl s5sets/dta.jsonl; } >"$A/all-refs.jsonl"
jq -c 'select(.source=="bln600" or .source=="dta19")' "$A/all-refs.jsonl" >"$A/new-refs.jsonl"
if [ -s results/s5-new/new-baseline.jsonl ] && [ -s results/s5-new/new-adapter.jsonl ]; then
  cat results/s5-new/new-adapter.jsonl results/s5-new2/new-adapter.jsonl results/s5-new3/new-adapter.jsonl >"$A/new-adapter.jsonl"
  cp results/s5-new/new-baseline.jsonl "$A/new-baseline.jsonl"
  for r in baseline adapter; do $T guard-cut --input "$A/new-$r.jsonl" --out "$A/new-$r-stop.jsonl"; done
  H=(--hyp baseline=$A/new-baseline.jsonl --hyp baseline-stop=$A/new-baseline-stop.jsonl --hyp adapter=$A/new-adapter.jsonl --hyp adapter-stop=$A/new-adapter-stop.jsonl)
  $T score --refs "$A/new-refs.jsonl" "${H[@]}" --baseline baseline --out "$A/new.jsonl"
  $T score --refs "$A/new-refs.jsonl" --hyp baseline-stop=$A/new-baseline-stop.jsonl --hyp adapter-stop=$A/new-adapter-stop.jsonl --baseline baseline-stop --out "$A/new-guarded-diff.jsonl"
  $T invent --refs "$A/new-refs.jsonl" "${H[@]}" --out "$A/new-invent.jsonl"
fi
for n in stock2b lora2b; do [ -s results/s5-d2/$n.b64 ] || continue; cat results/s5-d2/$n.b64 results/s5-d2b/$n.b64 2>/dev/null >"$A/$n.b64"; scripts/decode.sh --kind qwen --in "$A/$n.b64" --out "$A/$n.jsonl"; $T guard-cut --input "$A/$n.jsonl" --out "$A/$n-stop.jsonl"; done
if [ -s "$A/lora2b-stop.jsonl" ]; then
  $T score --refs "$A/all-refs.jsonl" --hyp gguf-stock=$A/stock2b-stop.jsonl --hyp gguf-adapter=$A/lora2b-stop.jsonl --baseline gguf-stock --out "$A/gguf.jsonl"
  $T invent --refs "$A/all-refs.jsonl" --hyp gguf-stock=$A/stock2b-stop.jsonl --hyp gguf-adapter=$A/lora2b-stop.jsonl --out "$A/gguf-invent.jsonl"
  # export loss: same adapter, BF16 against GGUF
  cat "$A/test-baseline-stop.jsonl" "$A/new-baseline-stop.jsonl" >"$A/bf16-baseline-stop.jsonl" 2>/dev/null || true
  cat "$A/test-adapter-stop.jsonl" "$A/new-adapter-stop.jsonl" >"$A/bf16-adapter-stop.jsonl" 2>/dev/null || true
  $T score --refs "$A/all-refs.jsonl" --hyp bf16=$A/bf16-adapter-stop.jsonl --hyp gguf=$A/lora2b-stop.jsonl --baseline bf16 --out "$A/export-adapter.jsonl"
  $T score --refs "$A/all-refs.jsonl" --hyp bf16=$A/bf16-baseline-stop.jsonl --hyp gguf=$A/stock2b-stop.jsonl --baseline bf16 --out "$A/export-base.jsonl"
fi
f() { [ -s "$1" ] && jq -c 'select(.group|test("^source:|^all$"))|[.group,.reader,.items,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$1"; }
for x in new new-guarded-diff gguf export-adapter export-base; do echo "== $x"; f "$A/$x.jsonl" || true; done
for x in new-invent gguf-invent; do echo "== $x"; [ -s "$A/$x.jsonl" ] && jq -c 'select(.crops)|[.group,.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/$x.jsonl"; done
