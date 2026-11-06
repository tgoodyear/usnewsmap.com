#!/usr/bin/env bash
# Rows {id,ref,page,source,lang,image} for one benchmark subset, with the image path relative to the release.
set -euo pipefail
REL=""; SUBSET=""; OUT=""
while [ $# -gt 0 ]; do case "$1" in --release) REL=$2; shift 2;; --subset) SUBSET=$2; shift 2;; --out) OUT=$2; shift 2;; *) exit 2;; esac; done
jq -c --arg s "$SUBSET" '(.subsets|if type=="array" then map(select(.id==$s))[0] else .[$s] end) as $sub
  | ($sub.members|map(if type=="object" then .id else . end)) as $m
  | .samples[]|select(.id as $i|$m|index($i))
  | {id,page:.source_page_id,source,lang:.languages[0],image:.image.path,refpath:.reference.path}' "$REL/manifest.json" |
while IFS= read -r row; do
  p=$(jq -r .refpath <<<"$row"); jq -c --rawfile t "$REL/$p" 'del(.refpath)+{ref:$t}' <<<"$row"
done >"$OUT"
wc -l "$OUT"
