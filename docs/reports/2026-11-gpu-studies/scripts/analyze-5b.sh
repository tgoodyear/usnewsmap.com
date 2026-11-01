#!/usr/bin/env bash
# Study 5 step 1 scores: the chosen guard applied to finished readings on our side, checked against the live guard on validation.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s5; mkdir -p "$A"
# check: offline cut against the live stop on validation
$T guard-cut --input results/s2-1/val-baseline.jsonl --out "$A/val-cut.jsonl"
jq -n --slurpfile a "$A/val-cut.jsonl" --slurpfile b results/s5-val/val-stop.jsonl '($b[0:0]) as $z | ([$b[]|{(.id):.text}]|add) as $m | {crops:($a|length), same:([$a[]|select(($m[.id]|tostring|gsub("\\s+$";""))==(.text|gsub("\\s+$";"")))]|length)}'
$T guard-cut --input results/s2-2/test-baseline.jsonl --out "$A/test-baseline-stop.jsonl"
$T guard-cut --input results/s2-2/test-adapter.jsonl --out "$A/test-adapter-stop.jsonl"
H=(--hyp baseline=results/s2-2/test-baseline.jsonl --hyp baseline-stop=$A/test-baseline-stop.jsonl --hyp adapter=results/s2-2/test-adapter.jsonl --hyp adapter-stop=$A/test-adapter-stop.jsonl)
$T score --refs lora/test-refs.jsonl "${H[@]}" --baseline baseline --out "$A/test-second-use.jsonl"
$T score --refs lora/test-refs.jsonl --hyp baseline-stop=$A/test-baseline-stop.jsonl --hyp adapter-stop=$A/test-adapter-stop.jsonl --baseline baseline-stop --out "$A/test-guarded-diff.jsonl"
$T invent --refs lora/test-refs.jsonl "${H[@]}" --out "$A/test-invent.jsonl"
$T guard-cut --input analysis/s1/qwen.jsonl --out "$A/qwen8b-stop.jsonl"
$T score --refs analysis/s1/refs.jsonl --hyp qwen-8b=analysis/s1/qwen.jsonl --hyp qwen-8b-stop=$A/qwen8b-stop.jsonl --hyp frak2021-psm3=analysis/s1/frak2021-psm3.jsonl --baseline frak2021-psm3 --out "$A/s1-second-use.jsonl"
$T invent --refs analysis/s1/refs.jsonl --hyp qwen-8b=analysis/s1/qwen.jsonl --hyp qwen-8b-stop=$A/qwen8b-stop.jsonl --out "$A/s1-invent.jsonl"
jq -r 'select(.id|startswith("pub-")|not)|.looped//false' "$A/qwen8b-stop.jsonl" | sort | uniq -c
jq -r 'select(.finish=="length")|select(.id|startswith("pub-")|not)|.looped//false' "$A/qwen8b-stop.jsonl" | sort | uniq -c
f() { jq -c 'select(.group|test("^source:|^all$"))|[.group,.reader,.items,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$1"; }
f "$A/test-second-use.jsonl"; f "$A/test-guarded-diff.jsonl"; f "$A/s1-second-use.jsonl"
jq -c 'select(.crops)|[.group,.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/test-invent.jsonl" "$A/s1-invent.jsonl"
