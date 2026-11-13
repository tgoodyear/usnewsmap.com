#!/usr/bin/env bash
# Run one BOINC bridge step on the on-prem BOINC host (#942), in place of the Azure bridge job
# that boinc/scripts/start-boinc-bridge.sh starts. Same arguments, so the priority controller's
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
# holds the lock. The host may opt into bounded queuing with
# USNM_BOINC_STEP_LOCK_WAIT_SECS (0..300; default 0). A caller may ask for longer for one step
# with --lock-wait-secs N (0..1200), which replaces that setting for the step. Waiting consumes
# the --wait-mins budget rather than extending it. --what-if prints the settings and runs nothing.
# --target is bridge, publish or handoff.
#
# quality-stage is the exception to that lock (#1415): it takes a lock of its own, so one stage
# runs at a time while the other steps go on. It uses no database, secrets or project files and
# writes only its campaign's tree, where a unit counts once its quality.json is in place and
# later steps wait for that or for `submitted`. It takes about two minutes per 200 pages (#529).
# $CONF/stage-image may name another image for it, as sha256:<image id>, like handoff-image.
#
# The hand-off steps are the other exception: they take a lock of their own, because they touch
# Azure storage, Cosmos and the queues only, and the controller, their one caller, runs one at a
# time, and so do spool-out and batch-inventory, the controller's two bridge steps that only read
# the project and the database and write only the hand-off container. So a long collect doesn't
# keep submits and publishing waiting, and an hour's native publication, which holds the step lock
# for 10 minutes, doesn't fail a tick after the lock wait.
#
# --target handoff --step claim|release|dead-letter|deadline-retry|collect|export|line-report runs the hand-off
# job's command (#967 phase 3),
# boinc/images/bridge/handoff-job.sh from the same bridge image, in place of the
# Azure hand-off job (infra/modules/boinc-handoff-job.bicep). Its settings are the job's own,
# in $CONF/handoff.env: HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL
# QUEUE_URL, plus the Arc identity endpoints IDENTITY_ENDPOINT and IMDS_ENDPOINT (the host's
# own Arc machine identity holds the job's grants, boinc-handoff-runner.bicep). $CONF/handoff-image
# may name another image for these steps, as sha256:<image id>, so a hand-off fix needn't wait for a
# server image. It gets
# HANDOFF_STEP and only the step's own inputs, mounts only the Arc agent's challenge
# directory: no project tree, no database, no Key Vault. texts-export still runs in Azure.
#
# --target scans --step receive (#1116) reads a tar of scans from stdin, as the controller's push
# sends them over ssh: copies of the scan fetcher's files, named keep/<batch>/<lccn>/<date>/<ed>/
# <seq>.<jp2|jpg> with their .json sidecars. A line export's staging tree (#1621, usnm-boinc
# line-stage) adds each page's LoC ALTO, loc-alto/<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml,
# and its prefetch manifest, manifests/<name>/manifest.parquet. Only that layout is taken
# (regular files, size capped); each is written to $USNM_BOINC_SCAN_STAGE (default
# /srv/usnm-boinc/project/handoff-scans) through a temporary name, and staged files older than 4
# days are pruned. The hand-off export reads them (mounted read-only as /scans), and the
# bridge's submit takes a job's page from them, so no scan goes through Azure. The BOINC host
# never reads /tank/ocr itself.
#
# --target archive --step bridge --var ARCHIVE_BATCH=<NNNN> (#835) runs archive_history.py's
# bridge step for one batch the controller host's held-page archive dispatched
# (boinc/scripts/onprem/held-archive.sh, over ssh). It reads a tar of that batch's manifest.json
# and receipts/dispatch.json from stdin, and nothing else, into
# $USNM_BOINC_ARCHIVE_ROOT/batches/<NNNN> (default /srv/usnm-boinc/project/archive). A batch
# already there must have the same two files. Then it runs the tool installed there,
# $USNM_BOINC_ARCHIVE_ROOT/archive_history.py, as a dry run and then with --apply. The tool
# holds the bridge step locks itself, so this step takes none of them. A batch whose bridge
# receipt already names the same manifest is done: the step prints that receipt and changes
# nothing. The db step stays an operator's, as root on the host (#1587).
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
# (boinc/scripts/batch_scheduler.py). Run as root on the host, any step input is allowed, but
# never a variable that steers the shell, the loader or an interpreter in the container.
# JOB_UNITS and BUNDLE_APP_VERSION: OCR bundles (#1059), checked by bridge-job.sh.
# NATIVE_UNITS: native OCR units, each for one host (#1780). The controller picks the host, and
# usnm-boinc submit creates the unit only if the trust list admits that host.
SSH_VARS=" BATCH_NONCE BUNDLE_APP_VERSION HANDOFF_SPOOL HANDOFF_SPOOLS HANDOFF_URL JOB_ID JOB_IDS JOB_UNITS NATIVE_UNITS "
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
HANDOFF_DEAD_LETTER_VARS=" JOB_IDS SOURCE_TRUNCATED_DOCS "
# An operator's settlement of a page claimed again after its dead letter (#1417), as root on the
# host only: never the controller's key.
HANDOFF_DEAD_LETTER_OPERATOR_VARS="SUPERSEDES_FAILED_JOB CONFIRM_MESSAGE_SHA256 "
# Its other choice, retrying that page with another page deadline (#1417): root on the host only.
HANDOFF_DEADLINE_RETRY_VARS=" SUPERSEDES_FAILED_JOB TIMEOUT_SECS CONFIRM_MESSAGE_SHA256 "
HANDOFF_COLLECT_VARS=" JOB_ID JOB_IDS HOST_ID COLLECT_PARALLEL "
# The line jobs' check (#1621), as root on the host: it only reads.
HANDOFF_LINE_REPORT_VARS=" JOB_IDS "
HANDOFF_EXPORT_VARS=" HANDOFF_PAGES HANDOFF_BATCH HANDOFF_BATCHES HANDOFF_PLAN HANDOFF_MANIFEST ENGINE_IMAGE HANDOFF_SCANS HANDOFF_JOBS HANDOFF_CLAIMS HANDOFF_DEADLINE_RETRIES HANDOFF_LINE_READ "
# handoff.env must set these, as the Azure job sets them (its AZURE_CLIENT_ID is Container
# Apps' identity; the Arc identity needs none).
HANDOFF_HOST_VARS=" HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL QUEUE_URL IDENTITY_ENDPOINT IMDS_ENDPOINT "
CONTROL_VARS='^(BASH.*|ENV|SHELLOPTS|BASHOPTS|PS[0-9]|PROMPT_COMMAND|IFS|PATH|CDPATH|GLOBIGNORE|HOME|TMPDIR|LD_.*|PYTHON.*|PERL.*|RUBY.*|NODE_.*|LANG|LC_.*|TZ|HOSTALIASES|SSL_.*|CURL_.*|HTTPS?_PROXY|NO_PROXY|ALL_PROXY|IDENTITY_.*|IMDS_.*|AZURE_.*|MYSQL.*|RCLONE_.*)$'

CONF=${USNM_BOINC_CONF:-/etc/usnm-boinc}
# Where the step lock and the step's env file go (a scratch directory in the tests).
RUN_DIR=${USNM_BOINC_STEP_RUN:-/run}
LOCK_WAIT_ARG=""
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
    --lock-wait-secs)
      [ $# -ge 2 ] || usage
      [[ $2 =~ ^[0-9]{1,4}$ ]] && [ $((10#$2)) -le 1200 ] || die "--lock-wait-secs is 0..1200"
      LOCK_WAIT_ARG=$((10#$2)); shift 2 ;;
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
  archive)
    [ "$STEP" = bridge ] || die "--target archive takes --step bridge"
    [ -z "$APPVER_IMAGE" ] && [ ${#VARS[@]} -eq 1 ] && [[ ${VARS[0]} =~ ^ARCHIVE_BATCH=[0-9]{4}$ ]] ||
      die "--target archive takes --var ARCHIVE_BATCH=<NNNN> and nothing else" ;;
  publish)
    [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] || die "--target publish isn't allowed over ssh"
    [ "$STEP" = publish ] || die "--target publish takes --step publish"
    # A local image ID (sha256:<64 hex>, what boinc-app-release.sh builds and passes), or a reference.
    [[ $APPVER_IMAGE =~ ^sha256:[0-9a-f]{64}$ || $APPVER_IMAGE =~ ^[a-z0-9.-]+(:[0-9]+)?/[a-z0-9/_.-]+(:[A-Za-z0-9_.-]+)?(@sha256:[0-9a-f]{64})?$ ]] ||
      die "--target publish needs --image <appver image>" ;;
  handoff)
    [ -z "$APPVER_IMAGE" ] || die "--image is for --target publish"
    case $STEP in claim | release | dead-letter | deadline-retry | collect | export | line-report | lock-acquire | lock-renew | lock-release | lock-status) ;; *) die "--target handoff runs claim, release, dead-letter, deadline-retry, collect, export, line-report or lock-acquire|renew|release|status on-prem; $STEP stays in Azure" ;; esac
    [ "$STEP" != deadline-retry ] || [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] ||
      die "--step deadline-retry is an operator's, as root on the host; it isn't allowed over ssh"
    [ "$STEP" != line-report ] || [ "${USNM_BOINC_STEP_FROM_SSH:-}" != 1 ] ||
      die "--step line-report is an operator's, as root on the host; it isn't allowed over ssh" ;;
  *) die "--target $TARGET does not run on-prem; only bridge, publish, handoff, scans and archive steps do" ;;
esac
[[ $STEP =~ ^[a-z][a-z-]{0,40}$ ]] || die "--step is a bridge step name, such as register-apps or submit"

# The controller's local expiring lock (#1367, boinc/scripts/handoff_lock.py), kept in the
# hand-off root. lock-* steps run on the host with no container. A mutating hand-off step given
# LOCK_HOLDER and LOCK_GENERATION first checks that the stored record is still that holder's
# unexpired generation (fencing); the two never reach the container. Without them nothing changes.
HANDOFF_LOCK_ROOT=${USNM_BOINC_HANDOFF_ROOT:-/srv/usnm-boinc/handoff-state}
LOCK_TOOL=$(dirname "$(readlink -f "$0")")/handoff_lock.py
if [ "$TARGET" = handoff ]; then
  fence_holder="" fence_generation="" kept=() lock_args=()
  for v in ${VARS[@]+"${VARS[@]}"}; do
    case $v in
      LOCK_HOLDER=*) fence_holder=${v#*=} ;;
      LOCK_GENERATION=*) fence_generation=${v#*=} ;;
      LOCK_HOST=*) lock_args+=(--host "${v#*=}") ;;
      LOCK_PID=*) lock_args+=(--pid "${v#*=}") ;;
      LOCK_TTL=*) lock_args+=(--ttl "${v#*=}") ;;
      *) kept+=("$v") ;;
    esac
  done
  case $STEP in
    lock-*)
      [ ${#kept[@]} -eq 0 ] || die "--step $STEP takes only LOCK_HOLDER, LOCK_GENERATION, LOCK_HOST, LOCK_PID and LOCK_TTL"
      [ -f "$LOCK_TOOL" ] || die "no $LOCK_TOOL: install handoff_lock.py beside this script"
      [ -d "$HANDOFF_LOCK_ROOT" ] && [ ! -L "$HANDOFF_LOCK_ROOT" ] || die "no local hand-off root $HANDOFF_LOCK_ROOT"
      [ -z "$fence_holder" ] || lock_args+=(--holder "$fence_holder")
      [ -z "$fence_generation" ] || lock_args+=(--generation "$fence_generation")
      $WHAT_IF && { echo "would run $LOCK_TOOL ${STEP#lock-} on $HANDOFF_LOCK_ROOT"; exit 0; }
      exec python3 "$LOCK_TOOL" --root "$HANDOFF_LOCK_ROOT" "${STEP#lock-}" ${lock_args[@]+"${lock_args[@]}"} ;;
    claim | release | dead-letter | collect | export)
      if [ -n "$fence_holder$fence_generation" ]; then
        [ -n "$fence_holder" ] && [ -n "$fence_generation" ] || die "LOCK_HOLDER and LOCK_GENERATION go together"
        if ! $WHAT_IF; then
          python3 "$LOCK_TOOL" --root "$HANDOFF_LOCK_ROOT" fence --holder "$fence_holder" \
            --generation "$fence_generation" > /dev/null || die "the controller lock is not this controller's; $STEP not run"
        fi
      fi
      VARS=(${kept[@]+"${kept[@]}"}) ;;
    *) [ -z "$fence_holder$fence_generation" ] || die "--step $STEP is not fenced" ;;
  esac
fi

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
# A line export (#1621): LoC ALTO for each page, and a prefetch manifest of the pages.
alto_re = re.compile(r"loc-alto/[a-z0-9]{1,16}/[0-9]{4}/[0-9]{2}/[0-9]{2}/ed-[0-9]{1,4}/seq-[0-9]{1,5}/ocr\.xml")
manifest_re = re.compile(r"manifests/[A-Za-z0-9][A-Za-z0-9_-]{0,63}/manifest\.parquet")
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
        if m.isreg() and alto_re.fullmatch(m.name):
            cap = 32 * 1024 * 1024
        elif m.isreg() and manifest_re.fullmatch(m.name):
            cap = 16 * 1024 * 1024
        elif m.isreg() and name_re.fullmatch(m.name):
            cap = 64 * 1024 if m.name.endswith(".json") else 64 * 1024 * 1024
        else:
            sys.exit(f"error: {m.name!r} is not a staged scan, sidecar, LoC ALTO or line manifest")
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
# Prune what no job needs any more: a scan is read until its job is submitted, and the lease is 36 hours.
cutoff = time.time() - 4 * 86400
pruned = 0
for top in ("keep", "loc-alto", "manifests"):
    for root, dirs, files in os.walk(os.path.join(stage, top), topdown=False):
        for f in files:
            p = os.path.join(root, f)
            if os.lstat(p).st_mtime < cutoff:
                os.unlink(p)
                pruned += 1
        if root != os.path.join(stage, top) and not os.listdir(root):
            os.rmdir(root)
print(f"received {count} files ({total} bytes), pruned {pruned}")
' "$STAGE" "$RUN_DIR/usnm-boinc-scans.lock" "$WAIT_MINS" || die "the scans were not all received"
  exit 0
fi

ARCHIVE_ROOT=${USNM_BOINC_ARCHIVE_ROOT:-/srv/usnm-boinc/project/archive}
if [ "$TARGET" = archive ]; then
  batch=${VARS[0]#ARCHIVE_BATCH=}
  handoff_dir=${USNM_BOINC_ARCHIVE_HANDOFF_DIR:-/srv/usnm-boinc/project/handoff}
  results=${USNM_BOINC_ARCHIVE_RESULTS:-/srv/usnm-boinc/project/usnmocr/results}
  tool=$ARCHIVE_ROOT/archive_history.py
  if $WHAT_IF; then
    echo "would receive batch $batch's manifest and dispatch receipt from stdin into $ARCHIVE_ROOT/batches/$batch"
    echo "then: python3 $tool bridge --batch-dir $ARCHIVE_ROOT/batches/$batch --handoff-dir $handoff_dir --results $results [--apply --confirm-controller-paused]"
    exit 0
  fi
  [ -n "$WAIT_MINS" ] || die "--wait-mins is required on-prem"
  [ -f "$tool" ] && [ ! -L "$tool" ] || die "no $tool: install archive_history.py there first (docs/boinc/server/batch-scheduler.md)"
  mkdir -p "$ARCHIVE_ROOT"; [ -d "$ARCHIVE_ROOT/batches" ] || mkdir -m 0700 "$ARCHIVE_ROOT/batches"
  # One archive step at a time (a lock in the run directory). Reads the two files, checks them,
  # and puts them in place; prints "done" when the batch's bridge receipt already names the
  # manifest, else "new" or "same".
  state=$(python3 -c '
import fcntl, hashlib, json, os, signal, sys, tarfile, tempfile
root, batch, lock_path, wait = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
WANTED = {"manifest.json": 256 * 1024 * 1024, "receipts/dispatch.json": 1024 * 1024}


class Member(tarfile.TarInfo):
    # As for the scans: refuse a long PAX or GNU extension record before tarfile reads it.
    def _proc_member(self, tarfile_):
        if self.type in tarfile.GNU_TYPES + (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.SOLARIS_XHDTYPE) \
                and self.size > 4096:
            raise tarfile.HeaderError(f"a {self.size}-byte tar extension record")
        return super()._proc_member(tarfile_)


signal.alarm(wait * 60)
lock = open(lock_path, "w")
fcntl.flock(lock, fcntl.LOCK_EX)
batches = os.path.join(root, "batches")
got = {}
with tempfile.TemporaryDirectory(dir=batches) as tmp:
    with tarfile.open(fileobj=sys.stdin.buffer, mode="r|", tarinfo=Member) as tar:
        for index, m in enumerate(tar):
            if index >= 8:
                sys.exit("error: more tar entries than a batch sends")
            name = m.name[2:] if m.name.startswith("./") else m.name
            if m.isdir() and name.rstrip("/") in ("", ".", "receipts"):
                continue
            if not m.isreg() or name not in WANTED or name in got or m.size > WANTED[name]:
                sys.exit(f"error: {m.name!r} is not a batch manifest or dispatch receipt")
            path = os.path.join(tmp, name.replace("/", "_"))
            src = tar.extractfile(m)
            with open(path, "wb") as out:
                while chunk := src.read(1 << 20):
                    out.write(chunk)
            if os.path.getsize(path) != m.size:
                sys.exit(f"error: {m.name} came short")
            got[name] = path
    if set(got) != set(WANTED):
        sys.exit("error: the tar must hold manifest.json and receipts/dispatch.json")
    data = {name: open(path, "rb").read() for name, path in got.items()}
    receipt = json.loads(data["receipts/dispatch.json"])
    sha = hashlib.sha256(data["manifest.json"]).hexdigest()
    if not (isinstance(receipt, dict) and receipt.get("step") == "dispatch" and receipt.get("complete") is True
            and receipt.get("batch") == batch and receipt.get("manifest_sha256") == sha):
        sys.exit(f"error: the dispatch receipt is not batch {batch}'"'"'s complete receipt for this manifest")
    dest = os.path.join(batches, batch)
    if os.path.lexists(dest):
        if os.path.islink(dest) or not os.path.isdir(dest):
            sys.exit(f"error: {dest} is not a directory")
        for name, body in data.items():
            have = os.path.join(dest, name)
            if os.path.islink(have) or not os.path.isfile(have) or open(have, "rb").read() != body:
                sys.exit(f"error: batch {batch} is here with another {name}; stop and reconcile")
        done = os.path.join(dest, "receipts", "bridge.json")
        if os.path.isfile(done) and not os.path.islink(done):
            bridged = json.load(open(done))
            if bridged.get("complete") is True and bridged.get("manifest_sha256") == sha:
                print("done")
                sys.exit(0)
        print("same")
        sys.exit(0)
    staged = os.path.join(tmp, "batch")
    os.makedirs(os.path.join(staged, "receipts"), mode=0o700)
    os.chmod(staged, 0o700)
    for name, body in data.items():
        with open(os.path.join(staged, name), "wb") as out:
            out.write(body)
            out.flush()
            os.fsync(out.fileno())
    os.rename(staged, dest)
    print("new")
' "$ARCHIVE_ROOT" "$batch" "$RUN_DIR/usnm-boinc-archive.lock" "$WAIT_MINS")
  [ -n "$state" ] || die "batch $batch was not received"
  dir=$ARCHIVE_ROOT/batches/$batch
  if [ "$state" = "done" ]; then
    python3 -c '
import json, sys
r = json.load(open(sys.argv[1]))
r["workunits"] = len(r.get("workunits", []))
print(json.dumps({"event": "boinc_archive_bridge", "apply": True, "already": True, **r}, sort_keys=True))
' "$dir/receipts/bridge.json"
    exit 0
  fi
  bridge=(python3 "$tool" bridge --batch-dir "$dir" --handoff-dir "$handoff_dir" --results "$results")
  timeout --signal=TERM "${WAIT_MINS}m" "${bridge[@]}" || die "batch $batch's bridge dry run failed"
  timeout --signal=TERM "${WAIT_MINS}m" "${bridge[@]}" --apply --confirm-controller-paused ||
    die "batch $batch's bridge step failed; it resumes when run again"
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
# The hand-off steps may run a newer image of their own: they share nothing with the server.
if [ "$TARGET" = handoff ] && [ -f "$CONF/handoff-image" ]; then
  IMAGE=$(sed -n 's/^\(sha256:[0-9a-f]\{64\}\)$/\1/p' "$CONF/handoff-image")
  [ -n "$IMAGE" ] || die "$CONF/handoff-image is not sha256:<image id>"
fi
# So may quality-stage, which writes only its campaign's tree: $CONF/stage-image, for a staging fix.
if [ "$TARGET:$STEP" = bridge:quality-stage ] && [ -f "$CONF/stage-image" ]; then
  IMAGE=$(sed -n 's/^\(sha256:[0-9a-f]\{64\}\)$/\1/p' "$CONF/stage-image")
  [ -n "$IMAGE" ] || die "$CONF/stage-image is not sha256:<image id>"
fi
BASE=()
LINES=()
QUEUE_LOCATION=""
HANDOFF_LOCATION=""
# The last line counts even without a newline after it.
while IFS= read -r line || [ -n "$line" ]; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^([A-Z][A-Z0-9_]{0,63})=(.*)$ ]] || die "bad line in $SETTINGS"
  BASE+=("${BASH_REMATCH[1]}")
  LINES+=("$line")
  [ "${BASH_REMATCH[1]}" != QUEUE_URL ] || QUEUE_LOCATION=${BASH_REMATCH[2]}
  [ "${BASH_REMATCH[1]}" != HANDOFF_URL ] || HANDOFF_LOCATION=${BASH_REMATCH[2]}
  if [ "$TARGET" = handoff ]; then
    [[ $HANDOFF_HOST_VARS == *" ${BASH_REMATCH[1]} "* ]] || die "$SETTINGS sets ${BASH_REMATCH[1]}; it takes only$HANDOFF_HOST_VARS"
    [ -n "${BASH_REMATCH[2]}" ] || die "$SETTINGS has an empty ${BASH_REMATCH[1]}"
  fi
done < "$SETTINGS"
LAKE_MOUNTS=()
if [ "$TARGET" = handoff ]; then
  for line in "${LINES[@]}"; do
    case $line in
      CURATED_URL=*) store=curated ;;
      REFERENCE_URL=*) store=reference ;;
      OUT_URL=*) store=ocr-out ;;
      *) continue ;;
    esac
    location=${line#*=}
    case $location in
      https://*) continue ;;
      "/lake-state/$store") ;;
      *) die "local $store must use /lake-state/$store" ;;
    esac
    root=${USNM_BOINC_LAKE_ROOT:-/srv/usnm-boinc/lake-state}
    python3 - "$root" "$store" <<'PY' || die "invalid local lake store"
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
store = root / sys.argv[2]
info = root.stat()
if (not root.is_absolute() or root.resolve() != root or info.st_uid != os.geteuid()
        or info.st_mode & 0o777 != 0o700):
    raise SystemExit("local lake root must be canonical, owned by the runner user and mode 0700")
if not store.is_dir() or store.resolve() != store:
    raise SystemExit("local lake store must be an existing directory without symlinks")
PY
    LAKE_MOUNTS+=(-v "$root/$store:/lake-state/$store")
  done
fi
QUEUE_MOUNTS=()
if [ "$TARGET" = handoff ] && [[ $QUEUE_LOCATION == postgres://* || $QUEUE_LOCATION == postgresql://* ]]; then
  # Certificate paths in the URL are container paths, never caller-selected host mounts.
  for file in client.crt client.key ca.pem; do
    [ -f "$CONF/queue/$file" ] && [ -r "$CONF/queue/$file" ] ||
      die "pgmq requires readable $CONF/queue/$file"
    [ ! -L "$CONF/queue/$file" ] || die "pgmq certificate files must not be symlinks"
    QUEUE_MOUNTS+=(-v "$CONF/queue/$file:/etc/usnm/queue/$file:ro")
  done
fi
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
    dead-letter)
      SSH_ALLOWED=$HANDOFF_DEAD_LETTER_VARS
      [ "${USNM_BOINC_STEP_FROM_SSH:-}" = 1 ] || SSH_ALLOWED+=$HANDOFF_DEAD_LETTER_OPERATOR_VARS ;;
    deadline-retry) SSH_ALLOWED=$HANDOFF_DEADLINE_RETRY_VARS ;;
    collect) SSH_ALLOWED=$HANDOFF_COLLECT_VARS ;;
    export) SSH_ALLOWED=$HANDOFF_EXPORT_VARS ;;
    line-report) SSH_ALLOWED=$HANDOFF_LINE_REPORT_VARS ;;
  esac
  SSH_ALLOWED+=" EXPECTED_HANDOFF_URL "
fi
EXPECTED_HANDOFF_URL=""
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
  if [ "$TARGET" = handoff ] && [ "$name" = EXPECTED_HANDOFF_URL ]; then
    EXPECTED_HANDOFF_URL=${v#*=}
    [ -n "$EXPECTED_HANDOFF_URL" ] || die "EXPECTED_HANDOFF_URL must not be empty"
  fi
  if [ "$TARGET" = publish ]; then
    [[ $v =~ ^APPVER_REUSE_PROJECT=[01]$ ]] || die "--target publish takes only --var APPVER_REUSE_PROJECT=0 or 1"
    REUSE=${v#*=}
  fi
  seen+="$name "
done
if [ "$TARGET" = handoff ]; then
  if [ -n "$EXPECTED_HANDOFF_URL" ]; then
    [ "$EXPECTED_HANDOFF_URL" = "$HANDOFF_LOCATION" ] ||
      die "handoff backend mismatch between controller and host settings"
  elif [ "$HANDOFF_LOCATION" = /handoff-state ]; then
    die "local handoff requires EXPECTED_HANDOFF_URL from the controller"
  fi
fi

name="usnm-boinc-step-$(date -u +%Y%m%dT%H%M%SZ)-$STEP"
if $WHAT_IF; then
  if [ "$TARGET" = publish ]; then
    echo "would run $APPVER_IMAGE's publish-appver.sh as $name with $CONF/bridge.env and:"
    echo "  APPVER_REUSE_PROJECT=$REUSE"
  elif [ "$TARGET" = handoff ]; then
    echo "would run $IMAGE's handoff-job.sh as $name with $SETTINGS and:"
    echo "  HANDOFF_STEP=$STEP"
    if [ ${#QUEUE_MOUNTS[@]} -gt 0 ]; then
      echo "  pgmq certificates mounted read-only at /etc/usnm/queue"
    fi
    for v in ${VARS[@]+"${VARS[@]}"}; do echo "  $v"; done
  else
    echo "would run $IMAGE as $name with $CONF/bridge.env and:"
    echo "  BRIDGE_STEP=$STEP"
    for v in ${VARS[@]+"${VARS[@]}"}; do echo "  $v"; done
  fi
  exit 0
fi

[ -n "$WAIT_MINS" ] || die "--wait-mins is required on-prem"
lock_wait=${USNM_BOINC_STEP_LOCK_WAIT_SECS:-0}
[[ $lock_wait =~ ^[0-9]{1,3}$ ]] || die "USNM_BOINC_STEP_LOCK_WAIT_SECS must be 0..300"
lock_wait=$((10#$lock_wait))
[ "$lock_wait" -le 300 ] || die "USNM_BOINC_STEP_LOCK_WAIT_SECS must be 0..300"
[ -z "$LOCK_WAIT_ARG" ] || lock_wait=$LOCK_WAIT_ARG
step_timeout=$((WAIT_MINS * 60))
[ "$lock_wait" -lt "$step_timeout" ] || die "lock wait must be shorter than --wait-mins"
lock_file=usnm-boinc-step.lock
case $TARGET:$STEP in
  bridge:quality-stage) lock_file=usnm-boinc-stage.lock ;;
  handoff:* | bridge:spool-out | bridge:batch-inventory) lock_file=usnm-boinc-handoff.lock ;;
esac
exec 9> "$RUN_DIR/$lock_file"
lock_started=$SECONDS
if [ "$lock_wait" -eq 0 ]; then
  flock -n 9 || die "another bridge step is running on this host - wait for it to end first"
else
  flock -w "$lock_wait" 9 || die "another bridge step still holds the lock after ${lock_wait}s"
fi
step_timeout=$((step_timeout - (SECONDS - lock_started)))
[ "$step_timeout" -gt 0 ] || die "step deadline expired while waiting for the bridge lock"
# The wait may have outlasted the controller's lock: check the fence again before launching.
if [ -n "${fence_holder:-}" ]; then
  python3 "$LOCK_TOOL" --root "$HANDOFF_LOCK_ROOT" fence --holder "$fence_holder" \
    --generation "$fence_generation" > /dev/null || die "the controller lock expired while waiting for the step lock; $STEP not run"
fi
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
run+=(${QUEUE_MOUNTS[@]+"${QUEUE_MOUNTS[@]}"})
run+=(${LAKE_MOUNTS[@]+"${LAKE_MOUNTS[@]}"})
HANDOFF_ROOT=${USNM_BOINC_HANDOFF_ROOT:-/srv/usnm-boinc/handoff-state}
if [ -d "$HANDOFF_ROOT" ]; then
  [ ! -L "$HANDOFF_ROOT" ] || die "local handoff root must not be a symlink"
  python3 - "$HANDOFF_ROOT" <<'PY'
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
info = root.stat()
if (not root.is_absolute() or root.resolve() != root or info.st_uid != os.geteuid()
        or info.st_mode & 0o777 != 0o700):
    raise SystemExit("local handoff root must be canonical, owned by the runner user and mode 0700")
PY
  run+=(-v "$HANDOFF_ROOT:/handoff-state")
fi
# The hand-off steps touch Azure storage, Cosmos and the queues only: no project tree or database.
[ "$TARGET" = handoff ] || run+=(--add-host usnm-boinc-mysql:127.0.0.1
  -v /srv/usnm-boinc/project:/project
  -v /srv/usnm-boinc/mysql-tls/ca.pem:/usr/local/share/usnm/mysql-ca.pem:ro)
echo "running $STEP as $name for up to $step_timeout seconds after lock acquisition" >&2
rc=0
if [ "$TARGET" = handoff ]; then
  # export takes the scans the controller pushed here (#1116), read-only, instead of the
  # container's copies. Queue-fed exports, and line exports (#1621), whose pages, ALTO and
  # manifest are staged here: any other cohort export's scans still go to the container.
  local_export=false
  for v in ${VARS[@]+"${VARS[@]}"}; do
    case $v in HANDOFF_CLAIMS=1 | HANDOFF_LINE_READ=?*) local_export=true ;; esac
  done
  if [ "$STEP" = export ] && [ -d "$STAGE" ] && $local_export; then
    echo "HANDOFF_LOCAL_SCANS=1" >> "$envfile"
    run+=(-v "$STAGE:/scans:ro")
  fi
  "${run[@]}" --timeout "$step_timeout" --entrypoint /usr/local/lib/usnm-boinc/handoff-job.sh "$IMAGE" || rc=$?
elif [ "$TARGET" = publish ]; then
  "${run[@]}" --timeout "$step_timeout" --entrypoint /usr/local/lib/usnm-boinc/publish-appver.sh "$APPVER_IMAGE" || rc=$?
else
  "${run[@]}" --timeout "$step_timeout" "$IMAGE" || rc=$?
fi
[ $rc -eq 0 ] || die "$name ended with status $rc; its log is in journalctl -t usnm-boinc-step"
echo "$name succeeded" >&2
echo "$name"
