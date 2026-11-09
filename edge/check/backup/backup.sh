#!/usr/bin/env bash
# Nightly copy of check.usnewsmap.com's database (README.md in this directory).
#
#   backup.sh --token-file FILE --out-dir DIR [--base URL]
#
# --token-file: a file holding the Worker's admin token (mode 600).
# --out-dir: where dated copies go: DIR/YYYY-MM-DD/{dump.jsonl.gz,export-<lang>.jsonl.gz,SHA256SUMS}.
# Exits non-zero if the dump is missing, unparsable or has no answers, so the timer shows a failure.
set -euo pipefail
TOKEN_FILE="" OUT="" BASE=https://check.usnewsmap.com
while [ $# -gt 0 ]; do
  case "$1" in
    --token-file) TOKEN_FILE=$2; shift 2 ;;
    --out-dir) OUT=$2; shift 2 ;;
    --base) BASE=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -r "$TOKEN_FILE" ] && [ -n "$OUT" ] || { echo "usage: $0 --token-file FILE --out-dir DIR [--base URL]" >&2; exit 2; }

day=$(date +%F)
dir=$OUT/$day
tmp=$(mktemp -d "$OUT/.partial.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
hdr=$tmp/header
(umask 077; printf 'authorization: Bearer %s\n' "$(tr -d '\n' < "$TOKEN_FILE")" > "$hdr")
get() { curl -fsS --max-time 120 --retry 3 --retry-all-errors "$@"; }

get -H "@$hdr" "$BASE/admin/dump" -o "$tmp/dump.jsonl"
rm -f "$hdr"
# Every line parses, and the answers table isn't empty.
python3 - "$tmp/dump.jsonl" <<'PY'
import collections, json, sys
counts = collections.Counter(json.loads(line)["table"] for line in open(sys.argv[1], encoding="utf-8"))
print("rows:", dict(counts))
sys.exit(0 if counts["answers"] and counts["tasks"] and counts["languages"] else 1)
PY

for lang in $(python3 -c 'import json,sys; [print(json.loads(l)["row"]["code"]) for l in open(sys.argv[1]) if json.loads(l)["table"] == "languages"]' "$tmp/dump.jsonl"); do
  get "$BASE/api/export?lang=$lang" -o "$tmp/export-$lang.jsonl"
done
gzip -9 "$tmp"/*.jsonl
(cd "$tmp" && sha256sum ./*.gz > SHA256SUMS)
chmod 600 "$tmp"/*
rm -rf "$dir"
mv "$tmp" "$dir"
trap - EXIT
ln -sfn "$day" "$OUT/latest"
echo "saved $dir"
