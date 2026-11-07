#!/usr/bin/env bash
# Loads a language's tasks, clips, configuration and seed answers into check.usnewsmap.com
# (README.md, "Loading a language"). The files stay on the build host: this script reads the admin
# token from Key Vault and hands it to the host over ssh's stdin, and the host sends the files
# straight to the Worker.
#
#   edge/check/load.sh --vault NAME --host USER@HOST --dir DIR --lang CODE [--seed FILE] [--no-clips]
#
# DIR (on HOST) holds tasks.jsonl, language.json and clips/ from tools/from_tap_data.py.
# --seed: a JSON-lines file on HOST of {task, contributor: "seed:<name>", choice, at}.
# --no-clips: skip the images (when only the configuration or tasks changed).
set -euo pipefail
VAULT="" HOST="" DIR="" LANG_CODE="" SEED="" CLIPS=true
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=$2; shift 2 ;;
    --host) HOST=$2; shift 2 ;;
    --dir) DIR=$2; shift 2 ;;
    --lang) LANG_CODE=$2; shift 2 ;;
    --seed) SEED=$2; shift 2 ;;
    --no-clips) CLIPS=false; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$VAULT" ] && [ -n "$HOST" ] && [ -n "$DIR" ] && [[ "$LANG_CODE" =~ ^[a-z]{2,3}$ ]] || { echo "usage: $0 --vault NAME --host USER@HOST --dir DIR --lang CODE [--seed FILE]" >&2; exit 2; }

TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-check-admin-token --query value -o tsv)
[ -n "$TOKEN" ] || { echo "no admin token in $VAULT" >&2; exit 1; }
# The token and the settings go in on stdin, ahead of the script, so they are never on a command line.
{
  printf 'TOKEN=%q DIR=%q LANG_CODE=%q SEED=%q CLIPS=%q\n' "$TOKEN" "$DIR" "$LANG_CODE" "$SEED" "$CLIPS"
  cat <<'REMOTE'
set -euo pipefail
BASE=https://check.usnewsmap.com
hdr=$(mktemp); chmod 600 "$hdr"; trap 'rm -f "$hdr"' EXIT
printf 'authorization: Bearer %s\n' "$TOKEN" > "$hdr"; unset TOKEN
auth=(-H "@$hdr" --max-time 60 --retry 3 --retry-all-errors)
cd "$DIR"
n=0
$CLIPS && while IFS= read -r -d '' f; do
  key=${f#clips/}
  case "$f" in *.png) type=image/png ;; *) type=image/jpeg ;; esac
  curl -fsS -X PUT "${auth[@]}" -H "content-type: $type" --data-binary "@$f" "$BASE/admin/clips/$key" >/dev/null
  n=$((n + 1))
done < <(find clips -type f -print0)
echo "clips: $n"
curl -fsS -X PUT "${auth[@]}" -H "content-type: application/json" --data-binary @language.json "$BASE/admin/languages/$LANG_CODE"; echo
curl -fsS -X POST "${auth[@]}" --data-binary @tasks.jsonl "$BASE/admin/tasks"; echo
if [ -n "$SEED" ]; then
  split -l 15 "$SEED" /tmp/check-seed.
  for part in /tmp/check-seed.*; do curl -fsS -X POST "${auth[@]}" --data-binary "@$part" "$BASE/admin/answers"; echo; done
  rm -f /tmp/check-seed.*
fi
REMOTE
} | ssh "$HOST" bash -s
