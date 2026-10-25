#!/usr/bin/env bash
# Run one BOINC bridge step on the on-prem BOINC host (#942), in place of the Azure bridge job
# that scripts/boinc/start-boinc-bridge.sh starts. Same arguments, so the priority controller's
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
# lock is held for exactly the step's run. --what-if prints the settings and runs nothing.
# --target is bridge, publish or handoff.
#
# --target handoff --step claim|release|collect|export runs the hand-off job's command (#967 phase 3),
# contrib-compute/images/bridge/handoff-job.sh from the same bridge image, in place of the
# Azure hand-off job (infra/modules/boinc-handoff-job.bicep). Its settings are the job's own,
# in $CONF/handoff.env: HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL
# QUEUE_URL, plus the Arc identity endpoints IDENTITY_ENDPOINT and IMDS_ENDPOINT (the host's
# own Arc machine identity holds the job's grants, boinc-handoff-runner.bicep). It gets
# HANDOFF_STEP and only the step's own inputs, mounts only the Arc agent's challenge
# directory: no project tree, no database, no Key Vault. texts-export still runs in Azure.
#
# --target scans --step receive (#1116) reads a tar of scans from stdin, as the controller's push
# sends them over ssh: copies of the scan fetcher's files, named keep/<batch>/<lccn>/<date>/<ed>/
# <seq>.<jp2|jpg> with their .json sidecars. Only that layout is taken (regular files, size
# capped); each is written to $USNM_BOINC_SCAN_STAGE (default /srv/usnm-boinc/project/
# handoff-scans) through a temporary name, and staged files older than 10 days are pruned. The
# hand-off export reads them (mounted read-only as /scans), and the bridge's submit takes a job's
# page from them, so no scan goes through Azure. The BOINC host never reads /tank/ocr itself.
#
# --target publish --step publish --image <appver image or sha256:<id>> publishes an app version (#961), as the
# Azure publish job did: the image's own publish-appver.sh runs with the host's settings and
# mounts and APPVER_REUSE_PROJECT=1 (the only --var it takes, 0 or 1). It is refused over ssh.
#
# Called over ssh with a forced command (the controller's key may run nothing else), use
# --from-ssh: the arguments come from SSH_ORIGINAL_COMMAND, split without a shell.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

# Over ssh the controller's key may set only the inputs the controller's bridge steps take
# (scripts/boinc/batch_scheduler.py). Run as root on the host, any step input is allowed, but
# never a variable that steers the shell, the loader or an interpreter in the container.
# JOB_UNITS and BUNDLE_APP_VERSION: OCR bundles (#1059), checked by bridge-job.sh.
SSH_VARS=" BATCH_NONCE BUNDLE_APP_VERSION HANDOFF_SPOOL HANDOFF_SPOOLS HANDOFF_URL JOB_ID JOB_IDS JOB_UNITS "
# The Surya steps of the controller's Surya plan (#968) take their own, and only these.
SSH_SURYA_CROPS_VARS=" SURYA_CAMPAIGN QUALITY_CAMPAIGN SURYA_ENGINE_IMAGE SURYA_ENGINE_LLAMA SURYA_ENGINE_WEIGHTS SURYA_MAX SURYA_TIMEOUT_SECS "
SSH_SURYA_SUBMIT_VARS=" SURYA_CAMPAIGN SURYA_MAX "
# The controller's gold onboarding (#901): issue gold units to untrusted hosts, then admit the
# ones that pass. Never a threshold, a challenge hash, a target host or a spool.
SSH_GOLD_ONBOARD_VARS=" GOLD_CAMPAIGN GOLD_LANGUAGE GOLD_PROFILE GOLD_ENGINE_IMAGE GOLD_ENGINE_BASE GOLD_ENGINE_VERSION GOLD_MAX_HOSTS "
# The hand-off steps take the inputs the controller gives them (batch_scheduler.py's claim,
# release, collect and export), over ssh or not.
HANDOFF_CLAIM_VARS=" HANDOFF_MANIFEST CLAIM_NONCE CLAIM_MAX CLAIM_QUEUES CLAIM_LEASE_HOURS CLAIM_LANGUAGES "
HANDOFF_RELEASE_VARS=" HANDOFF_DOCS "
HANDOFF_DEAD_LETTER_VARS=" JOB_IDS "
HANDOFF_COLLECT_VARS=" JOB_ID JOB_IDS HOST_ID "
HANDOFF_EXPORT_VARS=" HANDOFF_PAGES HANDOFF_BATCH HANDOFF_BATCHES HANDOFF_PLAN HANDOFF_MANIFEST ENGINE_IMAGE HANDOFF_SCANS HANDOFF_JOBS HANDOFF_CLAIMS "
# handoff.env must set these, as the Azure job sets them (its AZURE_CLIENT_ID is Container
# Apps' identity; the Arc identity needs none).
HANDOFF_HOST_VARS=" HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL QUEUE_URL IDENTITY_ENDPOINT IMDS_ENDPOINT "
CONTROL_VARS='^(BASH.*|ENV|SHELLOPTS|BASHOPTS|PS[0-9]|PROMPT_COMMAND|IFS|PATH|CDPATH|GLOBIGNORE|HOME|TMPDIR|LD_.*|PYTHON.*|PERL.*|RUBY.*|NODE_.*|LANG|LC_.*|TZ|HOSTALIASES|SSL_.*|CURL_.*|HTTPS?_PROXY|NO_PROXY|ALL_PROXY|IDENTITY_.*|IMDS_.*|AZURE_.*|MYSQL.*|RCLONE_.*)$'

CONF=${USNM_BOINC_CONF:-/etc/usnm-boinc}
# Where the step lock and the step's env file go (a scratch directory in the tests).
RUN_DIR=${USNM_BOINC_STEP_RUN:-/run}
ENV_NAME="" STEP="" WAIT_MINS="" WHAT_IF=false TARGET=bridge FROM_SSH=false NARGS=$# APPVER_IMAGE=""
VARS=()
while [ $# -gt 0 ]; do
  case $1 in
    --from-ssh) FROM_SSH=true; shift ;;
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --step) [ $# -ge 2 ] || usage; STEP=$2; shift 2 ;;
    --target) [ $# -ge 2 ] || usage; TARGET=$2; shift 2 ;;
    --image) [ $# -ge 2 ] || usage; APPVER_IMAGE=$2; shift 2 ;;
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
case $TARGET in
  bridge) [ -z "$APPVER_IMAGE" ] || die "--image is for --target publish" ;;
  scans)
    [ "$STEP" = receive ] || die "--target scans takes --step receive"
    [ -z "$APPVER_IMAGE" ] && [ ${#VARS[@]} -eq 0 ] || die "--target scans takes no --image or --var" ;;
  publish)
    [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] || die "--target publish isn't allowed over ssh"
    [ "$STEP" = publish ] || die "--target publish takes --step publish"
    # A local image ID (sha256:<64 hex>, what boinc-app-release.sh builds and passes), or a reference.
    [[ $APPVER_IMAGE =~ ^sha256:[0-9a-f]{64}$ || $APPVER_IMAGE =~ ^[a-z0-9.-]+(:[0-9]+)?/[a-z0-9/_.-]+(:[A-Za-z0-9_.-]+)?(@sha256:[0-9a-f]{64})?$ ]] ||
      die "--target publish needs --image <appver image>" ;;
  handoff)
    [ -z "$APPVER_IMAGE" ] || die "--image is for --target publish"
    case $STEP in claim | release | dead-letter | collect | export) ;; *) die "--target handoff runs claim, release, dead-letter, collect or export on-prem; $STEP stays in Azure" ;; esac ;;
  *) die "--target $TARGET does not run on-prem; only bridge, publish and handoff steps do" ;;
esac
[[ $STEP =~ ^[a-z][a-z-]{0,40}$ ]] || die "--step is a bridge step name, such as register-apps or submit"

STAGE=${USNM_BOINC_SCAN_STAGE:-/srv/usnm-boinc/project/handoff-scans}
if [ "$TARGET" = scans ]; then
  if $WHAT_IF; then
    echo "would receive scans from stdin into $STAGE"
    exit 0
  fi
  [ -n "$WAIT_MINS" ] || die "--wait-mins is required on-prem"
  mkdir -p "$STAGE"
  # The tar comes on stdin; python3's tarfile reads it as a stream, member by member. One
  # receive at a time (a lock in the run directory), stopped after --wait-mins.
  python3 -c '
import fcntl, os, re, signal, sys, tarfile, time
stage, lock_path, wait = sys.argv[1], sys.argv[2], int(sys.argv[3])


class Member(tarfile.TarInfo):
    # tarfile reads a PAX or GNU long-name record into memory before it yields the member:
    # refuse any but a short one, so a stream cannot make it allocate more.
    def _proc_member(self, tarfile_):
        if self.type in tarfile.GNU_TYPES + (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.SOLARIS_XHDTYPE) \
                and self.size > 4096:
            raise tarfile.HeaderError(f"a {self.size}-byte tar extension record")
        return super()._proc_member(tarfile_)


signal.alarm(wait * 60)
lock = open(lock_path, "w")
fcntl.flock(lock, fcntl.LOCK_EX)
name_re = re.compile(r"keep/[a-z]{2,10}_[A-Za-z0-9_]+/[A-Za-z0-9_-]+/[0-9]{4}-[0-9]{2}-[0-9]{2}/[0-9]{1,4}/[0-9]{1,5}\.(jp2|jpg)(\.json)?")
count = total = members = 0
with tarfile.open(fileobj=sys.stdin.buffer, mode="r|", tarinfo=Member) as tar:
    for m in tar:
        # Every header counts, directories too (tarfile keeps each one): a push of 100 pages
        # has about 200 files and a few hundred directories.
        members += 1
        if members > 5000:
            sys.exit("error: more tar entries than a push sends")
        if m.isdir():
            continue
        if not m.isreg() or not name_re.fullmatch(m.name):
            sys.exit(f"error: {m.name!r} is not a staged scan or sidecar")
        cap = 64 * 1024 if m.name.endswith(".json") else 64 * 1024 * 1024
        count += 1
        total += m.size
        if m.size > cap or count > 1000 or total > 32 * 1024 ** 3:
            sys.exit(f"error: {m.name} is over its size limit")
        dest = os.path.join(stage, m.name)
        os.makedirs(os.path.dirname(dest), mode=0o755, exist_ok=True)
        tmp = f"{dest}.tmp-{os.getpid()}"
        try:
            src = tar.extractfile(m)
            with open(tmp, "wb") as out:
                while chunk := src.read(1 << 20):
                    out.write(chunk)
            if os.path.getsize(tmp) != m.size:
                sys.exit(f"error: {m.name} came short")
            os.chmod(tmp, 0o644)
            os.replace(tmp, dest)
        finally:
            # A short or broken stream leaves no partial file behind.
            if os.path.exists(tmp):
                os.unlink(tmp)
# Prune what no job needs any more: a lease is at most seven days.
cutoff = time.time() - 10 * 86400
pruned = 0
for root, dirs, files in os.walk(os.path.join(stage, "keep"), topdown=False):
    for f in files:
        p = os.path.join(root, f)
        if os.lstat(p).st_mtime < cutoff:
            os.unlink(p)
            pruned += 1
    if root != os.path.join(stage, "keep") and not os.listdir(root):
        os.rmdir(root)
print(f"received {count} files ({total} bytes), pruned {pruned}")
' "$STAGE" "$RUN_DIR/usnm-boinc-scans.lock" "$WAIT_MINS" || die "the scans were not all received"
  exit 0
fi

# The host's own settings, which no --var may replace.
[ -r "$CONF/server.env" ] && [ -r "$CONF/bridge.env" ] || die "no $CONF/server.env or $CONF/bridge.env"
SETTINGS=$CONF/bridge.env
if [ "$TARGET" = handoff ]; then
  SETTINGS=$CONF/handoff.env
  [ -r "$SETTINGS" ] || die "no $SETTINGS: the hand-off job's settings, for --target handoff"
fi
IMAGE=$(sed -n 's/^SERVER_IMAGE=\([A-Za-z0-9._/:@-]\{1,200\}\)$/\1/p' "$CONF/server.env")
[ -n "$IMAGE" ] || die "$CONF/server.env names no SERVER_IMAGE"
BASE=()
LINES=()
# The last line counts even without a newline after it.
while IFS= read -r line || [ -n "$line" ]; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^([A-Z][A-Z0-9_]{0,63})=(.*)$ ]] || die "bad line in $SETTINGS"
  BASE+=("${BASH_REMATCH[1]}")
  LINES+=("$line")
  if [ "$TARGET" = handoff ]; then
    [[ $HANDOFF_HOST_VARS == *" ${BASH_REMATCH[1]} "* ]] || die "$SETTINGS sets ${BASH_REMATCH[1]}; it takes only$HANDOFF_HOST_VARS"
    [ -n "${BASH_REMATCH[2]}" ] || die "$SETTINGS has an empty ${BASH_REMATCH[1]}"
  fi
done < "$SETTINGS"
if [ "$TARGET" = handoff ]; then
  for v in $HANDOFF_HOST_VARS; do
    [[ " ${BASE[*]} " == *" $v "* ]] || die "$SETTINGS doesn't set $v"
  done
fi
seen=" ${BASE[*]} BRIDGE_STEP HANDOFF_STEP HANDOFF_LOCAL_SCANS "
REUSE=1
case $STEP in
  surya-stage-crops) SSH_ALLOWED=$SSH_SURYA_CROPS_VARS ;;
  surya-submit) SSH_ALLOWED=$SSH_SURYA_SUBMIT_VARS ;;
  gold-onboard | gold-promote) SSH_ALLOWED=$SSH_GOLD_ONBOARD_VARS ;;
  *) SSH_ALLOWED=$SSH_VARS ;;
esac
if [ "$TARGET" = handoff ]; then
  case $STEP in
    claim) SSH_ALLOWED=$HANDOFF_CLAIM_VARS ;;
    release) SSH_ALLOWED=$HANDOFF_RELEASE_VARS ;;
    dead-letter) SSH_ALLOWED=$HANDOFF_DEAD_LETTER_VARS ;;
    collect) SSH_ALLOWED=$HANDOFF_COLLECT_VARS ;;
    export) SSH_ALLOWED=$HANDOFF_EXPORT_VARS ;;
  esac
fi
for v in ${VARS[@]+"${VARS[@]}"}; do
  name=${v%%=*}
  [[ $v == *=* ]] && [[ $name =~ ^[A-Z][A-Z0-9_]{0,63}$ ]] || die "--var is NAME=VALUE with an upper-case NAME: $v"
  [[ ${v#*=} != *$'\n'* ]] || die "--var $name has a newline"
  [[ $seen != *" $name "* ]] || die "--var $name is set by the host or given twice"
  [[ ! $name =~ $CONTROL_VARS ]] || die "--var $name would steer the shell or an interpreter; not allowed"
  [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] || [[ $SSH_ALLOWED == *" $name "* ]] ||
    die "--var $name is not a controller bridge-step input; over ssh $STEP takes only$SSH_ALLOWED"
  [ "$TARGET" != handoff ] || [[ $SSH_ALLOWED == *" $name "* ]] ||
    die "--var $name is not a hand-off $STEP input; it takes only$SSH_ALLOWED"
  if [ "$TARGET" = publish ]; then
    [[ $v =~ ^APPVER_REUSE_PROJECT=[01]$ ]] || die "--target publish takes only --var APPVER_REUSE_PROJECT=0 or 1"
    REUSE=${v#*=}
  fi
  seen+="$name "
done

name="usnm-boinc-step-$(date -u +%Y%m%dT%H%M%SZ)-$STEP"
if $WHAT_IF; then
  if [ "$TARGET" = publish ]; then
    echo "would run $APPVER_IMAGE's publish-appver.sh as $name with $CONF/bridge.env and:"
    echo "  APPVER_REUSE_PROJECT=$REUSE"
  elif [ "$TARGET" = handoff ]; then
    echo "would run $IMAGE's handoff-job.sh as $name with $SETTINGS and:"
    echo "  HANDOFF_STEP=$STEP"
    for v in ${VARS[@]+"${VARS[@]}"}; do echo "  $v"; done
  else
    echo "would run $IMAGE as $name with $CONF/bridge.env and:"
    echo "  BRIDGE_STEP=$STEP"
    for v in ${VARS[@]+"${VARS[@]}"}; do echo "  $v"; done
  fi
  exit 0
fi

[ -n "$WAIT_MINS" ] || die "--wait-mins is required on-prem"
exec 9> "$RUN_DIR/usnm-boinc-step.lock"
flock -n 9 || die "another bridge step is running on this host - wait for it to end first"
envfile=$(mktemp "$RUN_DIR/usnm-boinc-step.XXXXXX")
trap 'rm -f "$envfile"' EXIT
# The settings as parsed above, one per line, so a file without a final newline can't run its
# last setting into the step's own.
if [ "$TARGET" = publish ]; then
  { printf '%s\n' ${LINES[@]+"${LINES[@]}"}; echo "APPVER_REUSE_PROJECT=$REUSE"; } > "$envfile"
elif [ "$TARGET" = handoff ]; then
  { printf '%s\n' ${LINES[@]+"${LINES[@]}"}; echo "HANDOFF_STEP=$STEP"; for v in ${VARS[@]+"${VARS[@]}"}; do echo "$v"; done; } > "$envfile"
else
  { printf '%s\n' ${LINES[@]+"${LINES[@]}"}; echo "BRIDGE_STEP=$STEP"; for v in ${VARS[@]+"${VARS[@]}"}; do echo "$v"; done; } > "$envfile"
fi

run=(podman run --rm --name "$name" --network host
  --log-driver journald --log-opt tag=usnm-boinc-step --env-file "$envfile"
  -v /var/opt/azcmagent/tokens:/var/opt/azcmagent/tokens:ro)
# The hand-off steps touch Azure storage, Cosmos and the queues only: no project tree or database.
[ "$TARGET" = handoff ] || run+=(--add-host usnm-boinc-mysql:127.0.0.1
  -v /srv/usnm-boinc/project:/project
  -v /srv/usnm-boinc/mysql-tls/ca.pem:/usr/local/share/usnm/mysql-ca.pem:ro)
echo "running $STEP as $name for up to $WAIT_MINS minutes" >&2
rc=0
if [ "$TARGET" = handoff ]; then
  # export takes the scans the controller pushed here (#1116), read-only, instead of the
  # container's copies.
  # Queue-fed exports only: a cohort export's scans still go to the container.
  if [ "$STEP" = export ] && [ -d "$STAGE" ] && [[ " ${VARS[*]-} " == *" HANDOFF_CLAIMS=1 "* ]]; then
    echo "HANDOFF_LOCAL_SCANS=1" >> "$envfile"
    run+=(-v "$STAGE:/scans:ro")
  fi
  "${run[@]}" --timeout $((WAIT_MINS * 60)) --entrypoint /usr/local/lib/usnm-boinc/handoff-job.sh "$IMAGE" || rc=$?
elif [ "$TARGET" = publish ]; then
  "${run[@]}" --timeout $((WAIT_MINS * 60)) --entrypoint /usr/local/lib/usnm-boinc/publish-appver.sh "$APPVER_IMAGE" || rc=$?
else
  "${run[@]}" --timeout $((WAIT_MINS * 60)) "$IMAGE" || rc=$?
fi
[ $rc -eq 0 ] || die "$name ended with status $rc; its log is in journalctl -t usnm-boinc-step"
echo "$name succeeded" >&2
echo "$name"
