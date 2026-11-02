#!/usr/bin/env bash
# Turn engine result files into JSONL on our side. Input is untrusted: parsed by jq, never executed.
set -euo pipefail
KIND=""; IN=""; OUT=""
while [ $# -gt 0 ]; do case "$1" in --kind) KIND=$2; shift 2;; --in) IN=$2; shift 2;; --out) OUT=$2; shift 2;; *) exit 2;; esac; done
case "$KIND" in
  qwen) grep -aE $'^[A-Za-z0-9._:-]{1,200}\t[0-9]{1,9}\t[A-Za-z0-9+/=]*$' "$IN" | jq -Rc 'split("\t")|{id:.[0],ms:(.[1]|tonumber),r:((.[2]|@base64d|fromjson?)//{})}|{id,ms,text:.r.choices[0].message.content,finish:.r.choices[0].finish_reason,tokens:.r.usage.completion_tokens}|select(.text|type=="string")' >"$OUT";;
  plain) grep -aE $'^[A-Za-z0-9._:-]{1,200}\t[A-Za-z0-9+/=]*$' "$IN" | jq -Rc 'split("\t")|{id:.[0],text:((.[1]//"")|@base64d)}' >"$OUT";;
  *) exit 2;; esac
wc -l "$OUT"
