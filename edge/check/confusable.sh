#!/usr/bin/env bash
# Marks check.usnewsmap.com tasks (README.md, "Words whose readings look alike" and "Words cut from a merged line"), or reopens them.
# Run by hand from the operator's machine, after the fix is live. It reads the admin token from Key Vault and keeps it
# off the command line.
#
#   edge/check/confusable.sh --vault NAME --action mark|reopen --tasks FILE [--reason confusable_display|merged_line] [--site URL]
#
# FILE has one task id per line (blank lines and lines starting with # are skipped). `mark` sets each task's existing
# answers aside (kept, in answers_excluded, and excluded from settling and every figure), keeps a settled task's old
# outcome as outcome_withdrawn, and holds the task out of the queue; `reopen` takes a task marked confusable_display back
# into the queue for new answers. --reason (mark only; default confusable_display, as before) is why the tasks are marked:
# merged_line is for a line clip that covers two printed lines, and such a task is never reopened. Marking a marked task,
# or reopening a task that isn't held (or isn't confusable_display), does nothing. A confusable_display task can be marked
# merged_line (README.md says when that is refused). The reply lists what changed, what
# didn't and what isn't a task. With --reason merged_line the script fails if the reply does not echo that reason, which
# is what a Worker from before migration 0005 would do (it would mark the tasks confusable_display).
set -euo pipefail
VAULT="" ACTION="" TASKS="" REASON="" SITE=https://check.usnewsmap.com
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=${2:-}; shift 2 ;;
    --action) ACTION=${2:-}; shift 2 ;;
    --tasks) TASKS=${2:-}; shift 2 ;;
    --reason) REASON=${2:-}; shift 2 ;;
    --site) SITE=${2:-}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ $VAULT =~ ^[a-z0-9-]+$ ]] && [[ $ACTION =~ ^(mark|reopen)$ ]] && [ -f "$TASKS" ] && [[ $SITE =~ ^https://[a-z0-9.-]+$ ]] \
  && [[ -z $REASON || ( $ACTION == mark && $REASON =~ ^(confusable_display|merged_line)$ ) ]] \
  || { echo "usage: $0 --vault NAME --action mark|reopen --tasks FILE [--reason confusable_display|merged_line (mark only)] [--site URL]" >&2; exit 2; }

TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-check-admin-token --query value -o tsv)
[ -n "$TOKEN" ] || { echo "no admin token in $VAULT" >&2; exit 1; }
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
(umask 077; printf 'authorization: Bearer %s\n' "$TOKEN" > "$work/header"); unset TOKEN
python3 - "$ACTION" "$TASKS" "$REASON" > "$work/body.json" <<'PY'
import json, sys
action, path, reason = sys.argv[1], sys.argv[2], sys.argv[3]
ids = [l.strip() for l in open(path, encoding="utf-8") if l.strip() and not l.lstrip().startswith("#")]
body = {"action": action, "tasks": ids}
if reason:
    body["reason"] = reason
print(json.dumps(body))
PY
curl -fsS -X POST --max-time 120 -H "@$work/header" -H "content-type: application/json" --data-binary "@$work/body.json" "$SITE/admin/confusable" > "$work/reply.json"
cat "$work/reply.json"
echo
if [ -n "$REASON" ] && [ "$REASON" != confusable_display ]; then
  python3 - "$REASON" "$work/reply.json" <<'PY' || { echo "the site did not echo reason $REASON: it may predate migration 0005 and its Worker; the tasks may be marked confusable_display" >&2; exit 1; }
import json, sys
sys.exit(0 if json.load(open(sys.argv[2])).get("reason") == sys.argv[1] else 1)
PY
fi
