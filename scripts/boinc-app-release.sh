#!/usr/bin/env bash
# Build, stage, sign and publish one BOINC app version on the live server, in one command:
#
#   scripts/boinc-app-release.sh --env prod --app usnm_ocr --version 1.0 --signing-host HOST --what-if
#   scripts/boinc-app-release.sh --env prod --app usnm_ocr --version 1.0 --signing-host HOST
#
# usnm_surya (#516) and usnm_qwen (#522) have their own worker images and two plan classes, one per version:
#   scripts/boinc-app-release.sh --env prod --app usnm_surya --version 1.0 --plan-class docker_mt \
#     --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
#   scripts/boinc-app-release.sh --env prod --app usnm_qwen --version 1.0 --plan-class docker_nvidia_cuda_8b \
#     --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
#
# Steps (contrib-compute/docs/app-release.md):
# 1. Worker image. Volunteers pull it from the public GHCR package named, by digest, in
#    images.worker of contrib-compute/infra/boinc/env/<env>.json; .github/workflows/boinc-worker.yml
#    publishes it. This checks that an anonymous pull of that digest works. With images.worker
#    empty it builds the worker in ACR Tasks instead (a build check, cached by input key; ACR
#    has no anonymous pull, so volunteers can't use that copy) and stops.
# 2. Stage. On the signing host, scripts/boinc/install.py runs inside the server image with
#    --network=none and writes the version directory (wrapper, job.toml, a Dockerfile pinned to
#    the worker digest, version.xml).
# 3. Sign. On the signing host, BOINC's sign_executable runs in the server image with
#    --network=none, with the private key mounted read-only. Only the .sig files come back.
#    The key's dataset is passphrase-locked; when it is locked this stops and says how to
#    unlock it. It never unlocks it.
# 4. Publish. ACR Tasks bakes the version directory (staged again from the same inputs, every
#    signature checked against the public key) into usnm-boinc-appver:<app>-<version>;
#    provision-boinc.sh --publish-image deploys the publish job with it, and the job copies the
#    version onto the share and verifies registration. The digest is then written to
#    images.appver in the settings, to commit: the stack keeps the job on it from then on.
#
# The signing host is reached only over SSH (as root, for zfs and podman), and is named only by
# --signing-host. The server image gets to it with a short-lived ACR pull token. Nothing is
# built, staged or published on this machine: the .sig files and the committed sources are all
# that pass through it. Sources come from the committed HEAD.
#
# --what-if makes the read-only checks (settings, Azure, the worker image, the signing host's
# key status and podman) and prints the plan.
# --prepare-only stages, signs and builds, then records the appver pin for a release PR
# without deploying or starting the publish job.
set -euo pipefail
usage() {
  echo "usage: scripts/boinc-app-release.sh --env <prod|dev> --app <usnm_ndlocr|usnm_ocr|usnm_assess|usnm_gold|usnm_quality|usnm_surya|usnm_qwen|usnm_qwen_train|usnm_iris> --version <N.M> --signing-host HOST [--kind container|native] [--binary FILE [--binary-sha256 HEX] [--extra-file LOGICAL=PATH ...]] [--plan-class CLASS] [--worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:...] [--platform PLATFORM [--wrapper FILE --wrapper-sha256 HEX --wrapper-source-pin FILE]] [--signing-key-dir DIR] [--signing-dataset DATASET] [--prepare-only] [--acceptance-config FILE --acceptance-evidence FILE [--coordination-claim URL]] [--what-if]" >&2
  exit 2
}
ENV_NAME="" APP="" VERSION="" SIGNING_HOST="" KIND=container PLAN_CLASS="" PLAN_CLASS_SET=false WORKER_OVERRIDE="" KEY_DIR=/usbbak/signing/boinc KEY_DATASET=usbbak/signing WHAT_IF=false PREPARE_ONLY=false
PLATFORM=x86_64-pc-linux-gnu WRAPPER="" WRAPPER_SHA="" WRAPPER_PIN="" BINARY="" BINARY_SHA=""
ACCEPTANCE_CONFIG="" ACCEPTANCE_EVIDENCE="" COORDINATION_CLAIM=""
EXTRA_FILES=()
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --app) [ $# -ge 2 ] || usage; APP=$2; shift 2 ;;
    --version) [ $# -ge 2 ] || usage; VERSION=$2; shift 2 ;;
    --signing-host) [ $# -ge 2 ] || usage; SIGNING_HOST=$2; shift 2 ;;
    --kind) [ $# -ge 2 ] || usage; KIND=$2; shift 2 ;;
    --binary) [ $# -ge 2 ] || usage; BINARY=$2; shift 2 ;;
    --binary-sha256) [ $# -ge 2 ] || usage; BINARY_SHA=$2; shift 2 ;;
    --extra-file) [ $# -ge 2 ] || usage; EXTRA_FILES+=("$2"); shift 2 ;;
    --plan-class) [ $# -ge 2 ] || usage; PLAN_CLASS=$2; PLAN_CLASS_SET=true; shift 2 ;;
    --worker-image) [ $# -ge 2 ] || usage; WORKER_OVERRIDE=$2; shift 2 ;;
    --platform) [ $# -ge 2 ] || usage; PLATFORM=$2; shift 2 ;;
    --wrapper) [ $# -ge 2 ] || usage; WRAPPER=$2; shift 2 ;;
    --wrapper-sha256) [ $# -ge 2 ] || usage; WRAPPER_SHA=$2; shift 2 ;;
    --wrapper-source-pin) [ $# -ge 2 ] || usage; WRAPPER_PIN=$2; shift 2 ;;
    --signing-key-dir) [ $# -ge 2 ] || usage; KEY_DIR=$2; shift 2 ;;
    --signing-dataset) [ $# -ge 2 ] || usage; KEY_DATASET=$2; shift 2 ;;
    --acceptance-config) [ $# -ge 2 ] || usage; ACCEPTANCE_CONFIG=$2; shift 2 ;;
    --acceptance-evidence) [ $# -ge 2 ] || usage; ACCEPTANCE_EVIDENCE=$2; shift 2 ;;
    --coordination-claim) [ $# -ge 2 ] || usage; COORDINATION_CLAIM=$2; shift 2 ;;
    --prepare-only) PREPARE_ONLY=true; shift ;;
    --what-if) WHAT_IF=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] && [ -n "$APP" ] && [ -n "$VERSION" ] && [ -n "$SIGNING_HOST" ] || usage
die() { echo "error: $*" >&2; exit 1; }
if [ -n "$ACCEPTANCE_CONFIG$ACCEPTANCE_EVIDENCE" ]; then
  [ -n "$ACCEPTANCE_CONFIG" ] && [ -n "$ACCEPTANCE_EVIDENCE" ] || die "acceptance requires both --acceptance-config and --acceptance-evidence"
  ! $PREPARE_ONLY || die "--prepare-only cannot run acceptance of an unpublished candidate"
  ACCEPTANCE_CONFIG=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$ACCEPTANCE_CONFIG")
  ACCEPTANCE_EVIDENCE=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$ACCEPTANCE_EVIDENCE")
fi
case "$KIND" in
  container)
    $PLAN_CLASS_SET || PLAN_CLASS=docker
    [ -z "$BINARY$BINARY_SHA" ] && [ "${#EXTRA_FILES[@]}" -eq 0 ] || die "--binary and --extra-file are only valid with --kind native"
    case "$APP:$PLAN_CLASS" in
      usnm_ocr:docker | usnm_assess:docker | usnm_gold:docker | usnm_quality:docker) ;;
      usnm_ndlocr:docker | usnm_surya:docker_mt | usnm_surya:docker_nvidia_cuda | usnm_qwen:docker_mt_2b | usnm_qwen:docker_nvidia_cuda_8b | usnm_qwen_train:docker_nvidia_train | usnm_iris:docker_intel_opencl_4b)
        [ -n "$WORKER_OVERRIDE" ] || die "$APP has its own worker image: give --worker-image" ;;
      *) die "unsupported --app and --plan-class: Qwen uses docker_mt_2b (2B) or docker_nvidia_cuda_8b (8B); see scripts/boinc/install.py" ;;
    esac
    ;;
  native)
    $PLAN_CLASS_SET || PLAN_CLASS=""
    [ -z "$WRAPPER$WRAPPER_SHA$WRAPPER_PIN$WORKER_OVERRIDE" ] || die "--wrapper and --worker-image are only valid with --kind container"
    [ -n "$BINARY" ] && [ -f "$BINARY" ] || die "--binary must be an existing file when --kind native"
    if [ -n "$BINARY_SHA" ]; then
      [[ $BINARY_SHA =~ ^[0-9a-f]{64}$ ]] || die "--binary-sha256 must be 64 lower-case hex digits"
      python3 - --binary "$BINARY" --sha256 "$BINARY_SHA" << 'PY' || die "--binary SHA-256 verification failed"
import argparse, hashlib, pathlib, sys
p = argparse.ArgumentParser()
p.add_argument("--binary", required=True, type=pathlib.Path)
p.add_argument("--sha256", required=True)
a = p.parse_args()
sys.exit(0 if hashlib.sha256(a.binary.read_bytes()).hexdigest() == a.sha256 else 1)
PY
    fi
    case "$APP:$PLAN_CLASS" in
      usnm_quality: | usnm_qwen:mt | usnm_qwen:apple_gpu_8b | usnm_qwen:nvidia_cuda_8b | usnm_qwen:nvidia_cuda_2b | usnm_qwen:intel_vulkan_8b) ;;
      *) die "--app and --plan-class for --kind native must be usnm_quality with empty --plan-class, or usnm_qwen with mt, apple_gpu_8b, nvidia_cuda_8b, nvidia_cuda_2b or intel_vulkan_8b (scripts/boinc/install.py)" ;;
    esac
    ;;
  *)
    die "--kind must be container or native"
    ;;
esac
[ -z "$WORKER_OVERRIDE" ] || [[ $WORKER_OVERRIDE =~ ^ghcr\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}$ ]] ||
  die "--worker-image must be ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<64 hex>"
[[ $VERSION =~ ^[1-9][0-9]*\.[0-9]+$ ]] || die "--version must look like 1.0"
[[ $SIGNING_HOST =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] || die "--signing-host must be a host name"
[[ $KEY_DIR =~ ^/[A-Za-z0-9/._-]+$ ]] || die "--signing-key-dir must be an absolute path"
[[ $KEY_DATASET =~ ^[A-Za-z0-9/._-]+$ ]] || die "--signing-dataset must be a ZFS dataset name"
cd "$(dirname "$0")/.."
if [ "$KIND" = container ]; then
  case "$PLATFORM" in
    x86_64-pc-linux-gnu) [ -z "$WRAPPER$WRAPPER_SHA$WRAPPER_PIN" ] || die "Linux uses the wrapper built in the pinned server image" ;;
    windows_x86_64)
      [ "$PLAN_CLASS" = docker ] || die "$PLATFORM supports only --plan-class docker"
      [ -f "$WRAPPER" ] && [ -f "$WRAPPER_PIN" ] && [[ $WRAPPER_SHA =~ ^[0-9a-f]{64}$ ]] ||
        die "Windows requires --wrapper, --wrapper-sha256 and --wrapper-source-pin from the reviewed build artifact"
      python3 scripts/boinc/check-windows-wrapper.py --wrapper "$WRAPPER" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$WRAPPER_PIN" || die "Windows wrapper verification failed" ;;
    aarch64-unknown-linux-gnu)
      [ "$PLAN_CLASS" = docker ] || die "$PLATFORM supports only --plan-class docker"
      [ -f "$WRAPPER" ] && [ -f "$WRAPPER_PIN" ] && [[ $WRAPPER_SHA =~ ^[0-9a-f]{64}$ ]] ||
        die "Linux arm64 requires --wrapper, --wrapper-sha256 and --wrapper-source-pin from the reviewed build artifact"
      python3 scripts/boinc/check-linux-wrapper.py --wrapper "$WRAPPER" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$WRAPPER_PIN" || die "Linux arm64 wrapper verification failed" ;;
    arm64-apple-darwin|x86_64-apple-darwin)
      [ "$PLAN_CLASS" = docker ] || die "$PLATFORM supports only --plan-class docker"
      [ -f "$WRAPPER" ] && [ -f "$WRAPPER_PIN" ] && [[ $WRAPPER_SHA =~ ^[0-9a-f]{64}$ ]] ||
        die "Mac requires --wrapper, --wrapper-sha256 and --wrapper-source-pin from the reviewed build artifact"
      python3 scripts/boinc/check-mac-wrapper.py --wrapper "$WRAPPER" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$WRAPPER_PIN" || die "Mac wrapper verification failed" ;;
    *) die "unsupported --platform" ;;
  esac
else
  case "$PLATFORM" in
    x86_64-pc-linux-gnu|aarch64-unknown-linux-gnu|arm64-apple-darwin|x86_64-apple-darwin|windows_x86_64) ;;
    *) die "unsupported --platform" ;;
  esac
  case "$PLAN_CLASS:$PLATFORM" in
    :* | mt:* | apple_gpu_8b:arm64-apple-darwin | nvidia_cuda_8b:x86_64-pc-linux-gnu | nvidia_cuda_8b:aarch64-unknown-linux-gnu | nvidia_cuda_8b:windows_x86_64 | nvidia_cuda_2b:x86_64-pc-linux-gnu | nvidia_cuda_2b:aarch64-unknown-linux-gnu | nvidia_cuda_2b:windows_x86_64 | intel_vulkan_8b:x86_64-pc-linux-gnu | intel_vulkan_8b:windows_x86_64) ;;
    *) die "native --plan-class $PLAN_CLASS is not supported on $PLATFORM" ;;
  esac
  for spec in "${EXTRA_FILES[@]+"${EXTRA_FILES[@]}"}"; do
    [[ $spec == *=* ]] || die "--extra-file must have the form LOGICAL=PATH"
    logical=${spec%%=*}
    src_file=${spec#*=}
    [[ $logical =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$ ]] || die "invalid --extra-file logical name $logical"
    [ -f "$src_file" ] || die "missing --extra-file source $src_file"
  done
  if [ "$APP" = usnm_qwen ]; then
    required_server="llama-server"
    [ "$PLATFORM" != windows_x86_64 ] || required_server="llama-server.exe"
    for req_extra in "$required_server" "llama.lock" "weights.lock"; do
      found_extra=false
      for spec in "${EXTRA_FILES[@]+"${EXTRA_FILES[@]}"}"; do
        if [ "${spec%%=*}" = "$req_extra" ]; then
          found_extra=true
          break
        fi
      done
      $found_extra || die "native usnm_qwen on $PLATFORM requires --extra-file $req_extra=PATH"
    done
  fi
fi
if [ -n "$ACCEPTANCE_CONFIG" ]; then
  acceptance_args=(--env "$ENV_NAME" --config "$ACCEPTANCE_CONFIG" --app "$APP" --version "$VERSION" --platform "$PLATFORM" --plan-class "$PLAN_CLASS" --evidence "$ACCEPTANCE_EVIDENCE")
  python3 scripts/boinc/acceptance.py "${acceptance_args[@]}" --dry-run || die "invalid acceptance configuration"
  if ! $WHAT_IF && [ "$ENV_NAME" = prod ]; then
    [[ $COORDINATION_CLAIM =~ ^https://github\.com/tgoodyear/usnewsmap\.com/issues/529#issuecomment-[0-9]+$ ]] || die "production acceptance requires an active #529 --coordination-claim"
  fi
fi
# shellcheck source=scripts/lib/boinc.sh disable=SC1091
. scripts/lib/boinc.sh
# shellcheck source=scripts/lib/acr-build.sh
. scripts/lib/acr-build.sh
boinc_load_settings
boinc_az_check
[ -z "$WORKER_OVERRIDE" ] || WORKER_IMAGE=$WORKER_OVERRIDE
[ -n "$SERVER_IMAGE" ] || die "no server image in $BOINC_SETTINGS: scripts/build-boinc-images.sh --env $ENV_NAME first"
PUBLIC_KEY=$(jq -r .codeSignPublicKey "$BOINC_SETTINGS")
[ -n "$PUBLIC_KEY" ] || die "no codeSignPublicKey in $BOINC_SETTINGS"
[ -z "$(git status --porcelain -- "$BOINC_SETTINGS")" ] || die "$BOINC_SETTINGS has uncommitted changes: commit them first (this run deploys the stack and writes images.appver)"
# A real run deploys this checkout's provision-boinc.sh and Bicep, not only HEAD: refuse a tree
# that differs from HEAD. --what-if only reads, so it warns.
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
  $WHAT_IF || die "the working tree differs from HEAD (git status); a release deploys this checkout's Bicep, so commit or clean first"
  echo "warning: the working tree differs from HEAD; a real run refuses this" >&2
fi
sha=$(git rev-parse HEAD)
registry=$(jq -er .registry infra/names/"$ENV_NAME".json) || die "no registry in infra/names/$ENV_NAME.json"
APPVER_REPO=usnm-boinc-appver
APPVER_TAG="$APP-$VERSION"
[ "$KIND" = container ] || APPVER_TAG="$APPVER_TAG-native"
if [ -n "$PLAN_CLASS" ] && [ "$PLAN_CLASS" != docker ]; then
  APPVER_TAG="$APPVER_TAG-${PLAN_CLASS//_/-}"
fi
[ "$PLATFORM" = x86_64-pc-linux-gnu ] || APPVER_TAG="$APPVER_TAG-$PLATFORM"
if [ -n "$PLAN_CLASS" ]; then
  VERSION_SUBDIR="apps/$APP/$VERSION/${PLATFORM}__${PLAN_CLASS}"
else
  VERSION_SUBDIR="apps/$APP/$VERSION/${PLATFORM}"
fi
ssh_host() { ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$SIGNING_HOST" "$@"; }

# --- 1. Worker image -------------------------------------------------------------------------

# ghcr_public REF: true when an anonymous client can read REF's manifest (what a volunteer's
# Docker does first).
ghcr_public() {
  local ref=$1 repo digest token
  repo=${ref#ghcr.io/}; repo=${repo%@*}; digest=${ref#*@}
  token=$(curl -fsS "https://ghcr.io/token?scope=repository:$repo:pull" | jq -er .token) || return 1
  curl -fsS -H "Authorization: Bearer $token" \
    -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json' \
    "https://ghcr.io/v2/$repo/manifests/$digest" | \
    python3 scripts/boinc/check-worker-platform.py --platform "$PLATFORM"
}

# have REPOSITORY TAG / digest REPOSITORY TAG: as in build-boinc-images.sh.
have() { baz acr repository show --name "$registry" --image "$1:$2" -o none 2> /dev/null; }
digest() {
  local d
  d=$(baz acr repository show --name "$registry" --image "$1:$2" --query digest -o tsv) || die "no $1:$2 in $registry"
  [[ $d =~ ^sha256:[0-9a-f]{64}$ ]] || die "unexpected digest for $1:$2"
  printf '%s' "$d"
}

# build_worker_in_acr: the engine (ocr/engines/tesseract) and the worker (ocr/boinc/Containerfile)
# in ACR Tasks, each keyed on the git hashes of its inputs and built only when the key is new.
build_worker_in_acr() {
  local engine_key worker_key engine_ref inputs
  # The builds upload the working tree but are keyed on HEAD: a tree that differs from HEAD
  # (tracked or untracked) would be cached under a key that doesn't name what was built.
  [ -z "$(git status --porcelain --untracked-files=normal)" ] ||
    die "the working tree differs from HEAD (git status); the worker build check is keyed on HEAD, so commit or clean first"
  engine_key=$(git rev-parse HEAD:ocr/engines/tesseract | cut -c1-16)
  inputs=(HEAD:crates HEAD:Cargo.toml HEAD:Cargo.lock HEAD:.dockerignore HEAD:ocr/boinc/Containerfile)
  # The toolchain picks the compiler of the worker's cargo build, as in build-boinc-images.sh.
  if git rev-parse -q --verify HEAD:rust-toolchain.toml > /dev/null; then inputs+=(HEAD:rust-toolchain.toml); fi
  worker_key=$({ git rev-parse "${inputs[@]}"; echo "$engine_key"; } | git hash-object --stdin | cut -c1-16)
  if have usnm-boinc-tesseract "$engine_key"; then
    echo "usnm-boinc-tesseract:$engine_key is in $registry"
  else
    echo "building usnm-boinc-tesseract:$engine_key in $registry"
    acr_build_verified baz_build "$registry" "usnm-boinc-tesseract:$engine_key" 7200 --platform linux/amd64 \
      --file ocr/engines/tesseract/Containerfile ocr/engines/tesseract || die "the engine image build failed"
  fi
  engine_ref="$registry.azurecr.io/usnm-boinc-tesseract@$(digest usnm-boinc-tesseract "$engine_key")"
  if have usnm-boinc-worker "$worker_key"; then
    echo "usnm-boinc-worker:$worker_key is in $registry"
  else
    echo "building usnm-boinc-worker:$worker_key in $registry (usnm-boinc release build: about 10 minutes)"
    acr_build_verified baz_build "$registry" "usnm-boinc-worker:$worker_key" 7200 --platform linux/amd64 \
      --build-arg "TESSERACT_IMAGE=$engine_ref" --file ocr/boinc/Containerfile . ||
      die "the worker image build failed"
  fi
  echo "worker image builds: $registry.azurecr.io/usnm-boinc-worker@$(digest usnm-boinc-worker "$worker_key") (ACR, not pullable by volunteers)"
}

if [ "$KIND" = native ]; then
  echo "1. native binary $BINARY: no worker container image required"
elif [ -z "$WORKER_IMAGE" ] && $WHAT_IF; then
  echo "1. no images.worker in $BOINC_SETTINGS: a real run builds the worker in ACR as a check and stops there (publish it with .github/workflows/boinc-worker.yml)"
elif [ -z "$WORKER_IMAGE" ]; then
  build_worker_in_acr
  die "no images.worker in $BOINC_SETTINGS. Publish the worker to GHCR (gh workflow run boinc-worker.yml --ref main; docs/boinc.md, \"Publish the worker image\"), make the package public once, put its ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... in images.worker, commit, and run this again"
else
  ghcr_public "$WORKER_IMAGE" ||
    die "$WORKER_IMAGE is not anonymously readable or lacks the required native Linux architecture for $PLATFORM"
  if [ "$APP" = usnm_qwen ]; then
    model=2b
    [ "$PLAN_CLASS" != docker_nvidia_cuda_8b ] || model=8b
    python3 scripts/boinc/qwen_disk.py --image "$WORKER_IMAGE" --model "$model" ||
      die "Qwen image model/disk policy does not match the plan class"
  fi
  echo "1. worker image $WORKER_IMAGE: anonymous pull works"
fi

# --- Signing host checks (read-only) ---------------------------------------------------------

keystatus=$(ssh_host "zfs get -H -o value keystatus $KEY_DATASET") || die "can't read the key status of $KEY_DATASET on the signing host over SSH"
if [ "$keystatus" != available ]; then
  die "the signing key's dataset $KEY_DATASET is locked ($keystatus). On the signing host, run:
    zfs load-key $KEY_DATASET && zfs mount $KEY_DATASET
then run this again. This script never unlocks it."
fi
[ "$(ssh_host "zfs get -H -o value mounted $KEY_DATASET")" = yes ] ||
  die "$KEY_DATASET has its key loaded but is not mounted: on the signing host, run zfs mount $KEY_DATASET"
ssh_host "test -r $KEY_DIR/code_sign_private" || die "no readable $KEY_DIR/code_sign_private on the signing host"
ssh_host "command -v podman > /dev/null" || die "podman is not installed on the signing host; stage and sign run in the server image there"
echo "signing host: $KEY_DATASET unlocked, key present, podman available"

if $WHAT_IF; then
  cat << PLAN
would, from commit $sha:
2. stage $APP $VERSION ($KIND, $PLATFORM, ${PLAN_CLASS:-default}) on the signing host: pull $SERVER_IMAGE (short-lived ACR token), run
   scripts/boinc/install.py in it with --network=none
3. sign each file with sign_executable (--network=none, key $KEY_DIR read-only); bring back the .sig files
4. build $registry.azurecr.io/$APPVER_REPO:$APPVER_TAG in ACR Tasks (every signature verified)
PLAN
  if $PREPARE_ONLY; then
    echo "   write images.appver for a release PR; do not deploy or start the publish job"
  else
    echo "   scripts/provision-boinc.sh --env $ENV_NAME --publish-image <it>, then start the publish job"
  fi
  exit 0
fi

work=$(boinc_workdir)
remote=""
boinc_acquire_lock "boinc-app-release.sh --env $ENV_NAME --app $APP --version $VERSION"
cleanup() {
  [ -z "$remote" ] || ssh_host "rm -rf $remote" || echo "warning: could not remove $remote on the signing host" >&2
  rm -rf "$work"
  boinc_release_lock
}
trap cleanup EXIT
binary_name="binary_payload"
[ "$PLATFORM" != windows_x86_64 ] || binary_name="binary_payload.exe"
if [ "$KIND" = native ]; then
  cp "$BINARY" "$work/$binary_name"
  if [ "${#EXTRA_FILES[@]}" -gt 0 ]; then
    mkdir -p "$work/extra"
    for spec in "${EXTRA_FILES[@]}"; do
      [[ $spec == *=* ]] || die "--extra-file must have the form LOGICAL=PATH"
      logical=${spec%%=*}
      src_file=${spec#*=}
      [ -f "$src_file" ] || die "missing --extra-file source $src_file"
      cp "$src_file" "$work/extra/$logical"
    done
  fi
elif [ -n "$WRAPPER" ]; then
  cp "$WRAPPER" "$work/wrapper"
  cp "$WRAPPER_PIN" "$work/wrapper.pin"
  case "$PLATFORM" in
    windows_x86_64)
      python3 scripts/boinc/check-windows-wrapper.py --wrapper "$work/wrapper" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$work/wrapper.pin" || die "Windows wrapper changed since verification" ;;
    aarch64-unknown-linux-gnu)
      python3 scripts/boinc/check-linux-wrapper.py --wrapper "$work/wrapper" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$work/wrapper.pin" || die "Linux arm64 wrapper changed since verification" ;;
    arm64-apple-darwin|x86_64-apple-darwin)
      python3 scripts/boinc/check-mac-wrapper.py --wrapper "$work/wrapper" --platform "$PLATFORM" \
        --sha256 "$WRAPPER_SHA" --source-pin "$work/wrapper.pin" || die "Mac wrapper changed since verification" ;;
  esac
fi

# --- 2. Stage --------------------------------------------------------------------------------

remote=$(ssh_host "python3 -c 'from pathlib import Path; import uuid; p=Path.cwd() / (\".usnm-appver-\"+uuid.uuid4().hex); p.mkdir(mode=0o700); print(p)'") || die "can't make a work directory on the signing host"
[[ $remote =~ ^/root/\.usnm-appver-[a-f0-9]{32}$ ]] || die "unexpected work directory $remote"
echo "2. staging on the signing host ($remote)"
baz acr login --name "$registry" --expose-token --query accessToken -o tsv |
  ssh_host "podman login --authfile $remote/auth.json -u 00000000-0000-0000-0000-000000000000 --password-stdin $registry.azurecr.io > /dev/null" ||
  die "can't log the signing host in to $registry"
ssh_host "podman pull -q --authfile $remote/auth.json $SERVER_IMAGE > /dev/null; rc=\$?; rm -f $remote/auth.json; exit \$rc" ||
  die "the signing host can't pull $SERVER_IMAGE"
git archive --format=tar HEAD scripts/boinc/install.py ocr/boinc contrib-compute/boinc/project.xml | ssh_host "mkdir $remote/src && tar -x -C $remote/src" ||
  die "can't copy the sources to the signing host"
if [ "$KIND" = native ]; then
  ssh_host "cat > $remote/$binary_name" < "$work/$binary_name" || die "can't copy the native binary to the signing host"
  extra_mount=""
  extra_arg=""
  if [ -d "$work/extra" ]; then
    tar -C "$work" -c extra | ssh_host "tar -x -C $remote" || die "can't copy extra files to the signing host"
    extra_mount="-v $remote/extra:/extra:ro"
    extra_arg="--bundle-dir /extra"
  fi
  ssh_host "mkdir -p $remote/proj && touch $remote/proj/config.xml &&
    podman run --rm --network=none -v $remote/$binary_name:/$binary_name:ro $extra_mount -v $remote/src:/src:ro -v $remote/proj:/proj --entrypoint python3 $SERVER_IMAGE \
      /src/scripts/boinc/install.py --kind native --project /proj --binary /$binary_name $extra_arg \
      --version $VERSION --app $APP --plan-class \"$PLAN_CLASS\" --platform $PLATFORM > /dev/null" || die "staging failed on the signing host"
else
  wrapper_path=/usr/local/lib/usnm-boinc/docker_wrapper
  wrapper_mount=""
  if [ -f "$work/wrapper" ]; then
    ssh_host "cat > $remote/wrapper" < "$work/wrapper" || die "can't copy the verified wrapper"
    wrapper_path=/wrapper
    wrapper_mount="-v $remote/wrapper:/wrapper:ro"
  fi
  ssh_host "mkdir -p $remote/proj && touch $remote/proj/config.xml &&
    podman run --rm --network=none $wrapper_mount -v $remote/src:/src:ro -v $remote/proj:/proj --entrypoint python3 $SERVER_IMAGE \
      /src/scripts/boinc/install.py --kind container --project /proj --wrapper $wrapper_path \
      --image $WORKER_IMAGE --version $VERSION --app $APP --plan-class $PLAN_CLASS --platform $PLATFORM > /dev/null" || die "staging failed on the signing host"
fi

# --- 3. Sign ---------------------------------------------------------------------------------

echo "3. signing on the signing host"
# shellcheck disable=SC2016 # expanded in the container
ssh_host "podman run --rm --network=none -v $KEY_DIR:/key:ro -v $remote/proj/$VERSION_SUBDIR:/v --entrypoint /bin/sh $SERVER_IMAGE -ec '
  cd /v
  for f in *; do
    case \$f in version.xml | *.sig) continue ;; esac
    /usr/local/boinc/tools/sign_executable \"\$f\" /key/code_sign_private > \"\$f.sig\"
    test -s \"\$f.sig\"
  done'" || die "signing failed on the signing host"
mkdir "$work/sigs"
ssh_host "cd $remote/proj/$VERSION_SUBDIR && tar -c *.sig" | tar -x -C "$work/sigs" || die "can't fetch the .sig files"
for f in "$work"/sigs/*; do [[ $(basename "$f") == *.sig ]] || die "unexpected file $(basename "$f") from the signing host"; done
echo "   signatures: $(cd "$work/sigs" && echo *)"

# --- 4. Publish ------------------------------------------------------------------------------

echo "4. building $APPVER_REPO:$APPVER_TAG in $registry"
ctx="$work/ctx"
mkdir -p "$ctx/src" "$ctx/extra"
git archive --format=tar HEAD scripts/boinc/install.py scripts/boinc/check-mac-wrapper.py scripts/boinc/check-linux-wrapper.py scripts/boinc/check-windows-wrapper.py contrib-compute/images/base/Containerfile ocr/boinc contrib-compute/boinc/project.xml | tar -x -C "$ctx/src"
git show "HEAD:contrib-compute/images/appver/Containerfile" > "$ctx/Containerfile"
git show "HEAD:contrib-compute/images/appver/publish-appver.sh" > "$ctx/publish-appver.sh"
git show "HEAD:contrib-compute/images/appver/check_appver.py" > "$ctx/check_appver.py"
mv "$work/sigs" "$ctx/sigs"
mkdir "$ctx/wrapper-candidate"
appver_wrapper=/usr/local/lib/usnm-boinc/docker_wrapper
appver_wrapper_pin=""
appver_binary=""
if [ "$KIND" = native ]; then
  cp "$work/$binary_name" "$ctx/wrapper-candidate/$binary_name"
  appver_binary="/opt/usnm-appver-build/wrapper-candidate/$binary_name"
  if [ -d "$work/extra" ]; then
    cp -r "$work/extra" "$ctx/wrapper-candidate/extra"
    cp "$work/extra"/* "$ctx/extra/"
  fi
elif [ -f "$work/wrapper" ]; then
  cp "$work/wrapper" "$ctx/wrapper-candidate/docker_wrapper"
  cp "$work/wrapper.pin" "$ctx/wrapper-candidate/wrapper.pin"
  appver_wrapper=/opt/usnm-appver-build/wrapper-candidate/docker_wrapper
  appver_wrapper_pin=/opt/usnm-appver-build/wrapper-candidate/wrapper.pin
fi
printf '%s' "$PUBLIC_KEY" > "$ctx/code_sign_public"
acr_build_verified baz_build "$registry" "$APPVER_REPO:$APPVER_TAG" 1800 --platform linux/amd64 \
  --build-arg "SERVER_IMAGE=$SERVER_IMAGE" --build-arg "KIND=$KIND" --build-arg "APP=$APP" --build-arg "VERSION=$VERSION" \
  --build-arg "WORKER_IMAGE=$WORKER_IMAGE" --build-arg "PLAN_CLASS=$PLAN_CLASS" --build-arg "PLATFORM=$PLATFORM" \
  --build-arg "WRAPPER_PATH=$appver_wrapper" --build-arg "WRAPPER_SHA256=$WRAPPER_SHA" --build-arg "WRAPPER_SOURCE_PIN=$appver_wrapper_pin" \
  --build-arg "BINARY_PATH=$appver_binary" --file "$ctx/Containerfile" "$ctx" ||
  die "the app version image build failed (a signature that doesn't verify fails it)"
appver_image="$registry.azurecr.io/$APPVER_REPO@$(digest "$APPVER_REPO" "$APPVER_TAG")"
echo "   $appver_image"

if ! $PREPARE_ONLY; then
  echo "   deploying and verifying the signed app version"
  scripts/provision-boinc.sh --env "$ENV_NAME" --publish-image "$appver_image"
  echo "published and verified $APP $VERSION ($PLATFORM, $PLAN_CLASS)"
fi
# Recorded like build-boinc-images.sh records the server and bridge: the stack keeps the publish
# job on the selected version once this is committed.
tmp="$work/settings.json"
jq --indent 2 --arg a "$appver_image" '.images.appver = $a' "$BOINC_SETTINGS" > "$tmp" && mv "$tmp" "$BOINC_SETTINGS"
echo "wrote images.appver to $BOINC_SETTINGS: commit it through a pull request, so the next provision-boinc.sh keeps the publish job on this version"
if $PREPARE_ONLY; then
  echo "prepared signed $APP $VERSION ($PLATFORM, $PLAN_CLASS); not published or registered"
  echo "after the release PR merges, run scripts/provision-boinc.sh --env $ENV_NAME from the merged checkout"
else
  echo "release registration and public health verified; volunteer execution is a separate acceptance measurement"
fi

if [ -n "${ACCEPTANCE_CONFIG:-}" ]; then
  python3 scripts/boinc/acceptance.py "${acceptance_args[@]}" --coordination-claim "$COORDINATION_CLAIM" ||
    die "published, but client acceptance failed: NOT release-ready (see private evidence)"
  echo "release-ready: expected client execution and validated canonical output verified"
fi
