#!/usr/bin/env bash
# Marks check.usnewsmap.com tasks confusable_display, or reopens them (README.md, "Words whose readings look alike").
# Run by hand from the operator's machine, after the fix is live. It reads the admin token from Key Vault and keeps it
# off the command line.
#
#   edge/check/confusable.sh --vault NAME --action mark|reopen --tasks FILE [--site URL]
#
# FILE has one task id per line (blank lines and lines starting with # are skipped). `mark` sets each task's existing
# answers aside (kept, in answers_excluded, and excluded from settling and every figure) and holds the task out of
# the queue; `reopen` takes a marked task back into the queue for new answers. Marking a marked task, or reopening a
# task that isn't held, does nothing. The reply lists what changed, what didn't and what isn't a task.
set -euo pipefail
VAULT="" ACTION="" TASKS="" SITE=https://check.usnewsmap.com
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=${2:-}; shift 2 ;;
    --action) ACTION=${2:-}; shift 2 ;;
    --tasks) TASKS=${2:-}; shift 2 ;;
    --site) SITE=${2:-}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ $VAULT =~ ^[a-z0-9-]+$ ]] && [[ $ACTION =~ ^(mark|reopen)$ ]] && [ -f "$TASKS" ] && [[ $SITE =~ ^https://[a-z0-9.-]+$ ]] \
  || { echo "usage: $0 --vault NAME --action mark|reopen --tasks FILE [--site URL]" >&2; exit 2; }

TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-check-admin-token --query value -o tsv)
[ -n "$TOKEN" ] || { echo "no admin token in $VAULT" >&2; exit 1; }
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
(umask 077; printf 'authorization: Bearer %s\n' "$TOKEN" > "$work/header"); unset TOKEN
python3 - "$ACTION" "$TASKS" > "$work/body.json" <<'PY'
import json, sys
action, path = sys.argv[1], sys.argv[2]
ids = [l.strip() for l in open(path, encoding="utf-8") if l.strip() and not l.lstrip().startswith("#")]
print(json.dumps({"action": action, "tasks": ids}))
PY
curl -fsS -X POST --max-time 120 -H "@$work/header" -H "content-type: application/json" --data-binary "@$work/body.json" "$SITE/admin/confusable"
echo
