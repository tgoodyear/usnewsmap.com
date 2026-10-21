#!/usr/bin/env bash
# Run one BOINC bridge step on the on-prem BOINC host (#942), in place of the Azure bridge job
# that scripts/start-boinc-bridge.sh starts. Same arguments, so the priority controller's
# step_command can point here:
#
#   onprem-step.sh --env prod --target bridge --step submit --var JOB_ID=<id> --wait-mins 30
#
# The step runs in a container from the bridge image named by SERVER_IMAGE in
# $USNM_BOINC_CONF/server.env (the image the server runs), with the settings in
# $USNM_BOINC_CONF/bridge.env (KEY_VAULT_NAME, DB_HOST, DB_NAME, MASTER_HOST, PROJECT_DIR,
# HANDOFF_DIR and the Arc identity endpoints) plus BRIDGE_STEP and each --var. It mounts the
# project tree, the database's CA and the Arc agent's challenge directory, as the server does.
#
# One step at a time, as the Azure job's start guard allows: a second caller fails while a step
# holds the lock. --wait-mins N (required) stops the step after N minutes and fails, so the
# lock is held for exactly the step's run. --what-if prints the settings and runs nothing. --target is bridge only: the hand-off job has no project tree and stays in Azure.
#
# Called over ssh with a forced command (the controller's key may run nothing else), use
# --from-ssh: the arguments come from SSH_ORIGINAL_COMMAND, split without a shell.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

# Over ssh the controller's key may set only the inputs the controller's bridge steps take
# (scripts/boinc/batch_scheduler.py). Run as root on the host, any step input is allowed, but
# never a variable that steers the shell, the loader or an interpreter in the container.
SSH_VARS=" BATCH_NONCE HANDOFF_SPOOL HANDOFF_SPOOLS HANDOFF_URL JOB_ID JOB_IDS "
CONTROL_VARS='^(BASH.*|ENV|SHELLOPTS|BASHOPTS|PS[0-9]|PROMPT_COMMAND|IFS|PATH|CDPATH|GLOBIGNORE|HOME|TMPDIR|LD_.*|PYTHON.*|PERL.*|RUBY.*|NODE_.*|LANG|LC_.*|TZ|HOSTALIASES|SSL_.*|CURL_.*|HTTPS?_PROXY|NO_PROXY|ALL_PROXY|IDENTITY_.*|IMDS_.*|AZURE_.*|MYSQL.*|RCLONE_.*)$'

CONF=${USNM_BOINC_CONF:-/etc/usnm-boinc}
ENV_NAME="" STEP="" WAIT_MINS="" WHAT_IF=false TARGET=bridge FROM_SSH=false NARGS=$#
VARS=()
while [ $# -gt 0 ]; do
  case $1 in
    --from-ssh) FROM_SSH=true; shift ;;
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --step) [ $# -ge 2 ] || usage; STEP=$2; shift 2 ;;
    --target) [ $# -ge 2 ] || usage; TARGET=$2; shift 2 ;;
    --var) [ $# -ge 2 ] || usage; VARS+=("$2"); shift 2 ;;
    --wait-mins)
      [ $# -ge 2 ] || usage
      [[ $2 =~ ^[0-9]+$ ]] && [ $((10#$2)) -ge 1 ] || die "--wait-mins is a number of minutes"
      WAIT_MINS=$((10#$2)); shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
if $FROM_SSH; then
  # The forced command is `onprem-step.sh --from-ssh` and nothing else; the real arguments are
  # the client's command, split without a shell, and checked like any others on the re-exec.
  [ "$NARGS" -eq 1 ] && [ -n "${SSH_ORIGINAL_COMMAND:-}" ] || die "--from-ssh takes its arguments from SSH_ORIGINAL_COMMAND"
  args=()
  while IFS= read -r a; do args+=("$a"); done < <(python3 -c 'import shlex, sys; print("\n".join(shlex.split(sys.argv[1])))' "$SSH_ORIGINAL_COMMAND")
  # The controller may send the command name first; drop it.
  case ${args[0]:-} in onprem-step.sh | */onprem-step.sh | usnm-boinc-step) args=("${args[@]:1}") ;; esac
  for a in ${args[@]+"${args[@]}"}; do [ "$a" != --from-ssh ] || die "--from-ssh once only"; done
  USNM_BOINC_STEP_FROM_SSH=1 exec "$0" ${args[@]+"${args[@]}"}
fi
[ -n "$ENV_NAME" ] && [ -n "$STEP" ] || usage
[[ $ENV_NAME =~ ^[a-z][a-z0-9-]{0,30}$ ]] || die "--env is an environment name"
[ "$TARGET" = bridge ] || die "--target $TARGET does not run on-prem; only bridge steps do"
[[ $STEP =~ ^[a-z][a-z-]{0,40}$ ]] || die "--step is a bridge step name, such as register-apps or submit"

# The host's own settings, which no --var may replace.
[ -r "$CONF/server.env" ] && [ -r "$CONF/bridge.env" ] || die "no $CONF/server.env or $CONF/bridge.env"
IMAGE=$(sed -n 's/^SERVER_IMAGE=\([A-Za-z0-9._/:@-]\{1,200\}\)$/\1/p' "$CONF/server.env")
[ -n "$IMAGE" ] || die "$CONF/server.env names no SERVER_IMAGE"
BASE=()
while IFS= read -r line; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^([A-Z][A-Z0-9_]{0,63})=(.*)$ ]] || die "bad line in $CONF/bridge.env"
  BASE+=("${BASH_REMATCH[1]}")
done < "$CONF/bridge.env"
seen=" ${BASE[*]} BRIDGE_STEP "
for v in ${VARS[@]+"${VARS[@]}"}; do
  name=${v%%=*}
  [[ $v == *=* ]] && [[ $name =~ ^[A-Z][A-Z0-9_]{0,63}$ ]] || die "--var is NAME=VALUE with an upper-case NAME: $v"
  [[ ${v#*=} != *$'\n'* ]] || die "--var $name has a newline"
  [[ $seen != *" $name "* ]] || die "--var $name is set by the host or given twice"
  [[ ! $name =~ $CONTROL_VARS ]] || die "--var $name would steer the shell or an interpreter; not allowed"
  [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] || [[ $SSH_VARS == *" $name "* ]] ||
    die "--var $name is not a controller bridge-step input; over ssh only$SSH_VARS"
  seen+="$name "
done

name="usnm-boinc-step-$(date -u +%Y%m%dT%H%M%SZ)-$STEP"
if $WHAT_IF; then
  echo "would run $IMAGE as $name with $CONF/bridge.env and:"
  echo "  BRIDGE_STEP=$STEP"
  for v in ${VARS[@]+"${VARS[@]}"}; do echo "  $v"; done
  exit 0
fi

[ -n "$WAIT_MINS" ] || die "--wait-mins is required on-prem"
exec 9> /run/usnm-boinc-step.lock
flock -n 9 || die "another bridge step is running on this host - wait for it to end first"
envfile=$(mktemp /run/usnm-boinc-step.XXXXXX)
trap 'rm -f "$envfile"' EXIT
{ cat "$CONF/bridge.env"; echo "BRIDGE_STEP=$STEP"; for v in ${VARS[@]+"${VARS[@]}"}; do echo "$v"; done; } > "$envfile"

run=(podman run --rm --name "$name" --network host --add-host usnm-boinc-mysql:127.0.0.1
  --log-driver journald --log-opt tag=usnm-boinc-step --env-file "$envfile"
  -v /srv/usnm-boinc/project:/project
  -v /srv/usnm-boinc/mysql-tls/ca.pem:/usr/local/share/usnm/mysql-ca.pem:ro
  -v /var/opt/azcmagent/tokens:/var/opt/azcmagent/tokens:ro)
echo "running $STEP as $name for up to $WAIT_MINS minutes" >&2
rc=0
"${run[@]}" --timeout $((WAIT_MINS * 60)) "$IMAGE" || rc=$?
[ $rc -eq 0 ] || die "$name ended with status $rc; its log is in journalctl -t usnm-boinc-step"
echo "$name succeeded" >&2
echo "$name"
