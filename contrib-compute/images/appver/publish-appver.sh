#!/usr/bin/env bash
# The publish job's command (infra/boinc/main.bicep, publishJob): copies the signed app version
# baked into this image (Containerfile) onto the project share, installs its templates, and runs
# the stock bin/update_versions, which reads the .sig files and adds the app_version rows.
#
# Platform versions are immutable: an existing platform directory must be identical,
# and then only update_versions runs again (it skips versions it already has).
set -euo pipefail
# shellcheck source=SCRIPTDIR/../server/lib.sh
. /usr/local/lib/usnm-boinc/lib.sh
: "${APPVER_APP:?}" "${APPVER_VERSION:?}"
APPVER_KIND=${APPVER_KIND:-container}
APPVER_PLATFORM=${APPVER_PLATFORM:-x86_64-pc-linux-gnu}
case "$APPVER_PLATFORM" in
  x86_64-pc-linux-gnu|aarch64-unknown-linux-gnu|arm64-apple-darwin|x86_64-apple-darwin|windows_x86_64) ;;
  *) fail "unsupported app platform" ;;
esac
case "$APPVER_KIND" in
  container)
    APPVER_PLAN_CLASS=${APPVER_PLAN_CLASS:-docker}
    case "$APPVER_APP:$APPVER_PLAN_CLASS" in
      usnm_ndlocr:docker|usnm_ocr:docker|usnm_assess:docker|usnm_gold:docker|usnm_quality:docker|usnm_surya:docker_mt|usnm_surya:docker_nvidia_cuda|usnm_qwen:docker_mt_2b|usnm_qwen:docker_nvidia_cuda_8b|usnm_qwen_train:docker_nvidia_train|usnm_iris:docker_intel_opencl_4b) ;;
      *) fail "unsupported container app/plan class: $APPVER_APP:$APPVER_PLAN_CLASS" ;;
    esac
    [ "$APPVER_PLATFORM" = x86_64-pc-linux-gnu ] || [ "$APPVER_PLAN_CLASS" = docker ] || fail "$APPVER_PLATFORM supports only plan class docker"
    subdir="${APPVER_PLATFORM}__${APPVER_PLAN_CLASS}"
    ;;
  native)
    APPVER_PLAN_CLASS=${APPVER_PLAN_CLASS-}
    case "$APPVER_APP:$APPVER_PLAN_CLASS" in
      usnm_quality:|usnm_qwen:mt|usnm_qwen:apple_gpu_8b|usnm_qwen:nvidia_cuda_8b|usnm_qwen:nvidia_cuda_2b) ;;
      *) fail "unsupported native app/plan class: $APPVER_APP:$APPVER_PLAN_CLASS" ;;
    esac
    case "$APPVER_PLAN_CLASS:$APPVER_PLATFORM" in
      :*|mt:*|apple_gpu_8b:arm64-apple-darwin|nvidia_cuda_8b:x86_64-pc-linux-gnu|nvidia_cuda_8b:aarch64-unknown-linux-gnu|nvidia_cuda_8b:windows_x86_64|nvidia_cuda_2b:x86_64-pc-linux-gnu|nvidia_cuda_2b:aarch64-unknown-linux-gnu|nvidia_cuda_2b:windows_x86_64) ;;
      *) fail "native plan class $APPVER_PLAN_CLASS is not supported on $APPVER_PLATFORM" ;;
    esac
    if [ -n "$APPVER_PLAN_CLASS" ]; then
      subdir="${APPVER_PLATFORM}__${APPVER_PLAN_CLASS}"
    else
      subdir="${APPVER_PLATFORM}"
    fi
    ;;
  *)
    fail "unsupported appver kind: $APPVER_KIND"
    ;;
esac
[ -e "$PROJ/.usnm-initialised" ] || fail "project is not initialised"
src=/usr/local/lib/usnm-boinc/appver
version_dir="$PROJ/apps/$APPVER_APP/$APPVER_VERSION"
dst="$version_dir/$subdir"
source_dir="$src/apps/$APPVER_APP/$APPVER_VERSION/$subdir"
# One publish at a time: executions of a manual job can overlap, and they share apps/,
# templates/ and update_versions. mkdir is atomic on the share; a lock older than the job's
# timeout is from a run that was killed, and is taken over.
lock="$PROJ/.usnm-publish.lock"
if ! mkdir "$lock" 2> /dev/null; then
  if [ -n "$(find "$lock" -maxdepth 0 -mmin +120 2> /dev/null)" ]; then
    # Rename is atomic: of two runs that both see the stale lock, only one moves it away, and
    # only that one may make the new lock (which mkdir again claims atomically).
    stale="$lock.stale.$$.$(date +%s)"
    mv "$lock" "$stale" 2> /dev/null || fail "another publish took over $lock first; wait for it to end"
    rm -rf "$stale"
    mkdir "$lock" 2> /dev/null || fail "another publish took $lock first; wait for it to end"
    log "took over $lock, left by a run more than 2 hours ago"
  else
    fail "another publish holds $lock; wait for it to end"
  fi
fi
registration=""
cleanup() {
  [ -z "$registration" ] || rm -rf "$registration"
  rmdir "$lock"
}
trap cleanup EXIT
mkdir -p "$PROJ/apps/$APPVER_APP" "$PROJ/templates"
has_existing_version=false
if [ -d "$version_dir" ] && [ -n "$(find "$version_dir" -mindepth 1 -maxdepth 1 -type d ! -name '.*' -print -quit)" ]; then
  has_existing_version=true
fi
# Adding a sibling platform or re-running a published version must not rewrite existing shared templates.
if [ "$has_existing_version" = true ]; then
  for t in "$src"/templates/*; do
    n=$(basename "$t")
    if [ -e "$PROJ/templates/$n" ]; then
      cmp -s "$t" "$PROJ/templates/$n" ||
        fail "templates/$n on the share differs from this image's; a changed template requires a new version"
    fi
  done
fi
if [ -e "$dst" ]; then
  diff -r "$source_dir" "$dst" > /dev/null ||
    fail "$dst is already on the share and differs from this image: versions are immutable, release a new version"
  log "$dst is already on the share and identical"
else
  mkdir -p "$version_dir"
  stage=$(mktemp -d "$version_dir/.$subdir.XXXXXX")
  cp -r "$source_dir/." "$stage/"
  chmod 0755 "$stage"
  mv "$stage" "$dst" || { rm -rf "$stage"; fail "can't move the version into $dst"; }
  log "copied $APPVER_APP $APPVER_VERSION to $dst"
fi
for t in "$src"/templates/*; do
  n=$(basename "$t")
  if [ -e "$PROJ/templates/$n" ] && cmp -s "$t" "$PROJ/templates/$n"; then
    continue
  fi
  tmp_tpl=$(mktemp "$PROJ/templates/.$n.XXXXXX")
  cp "$t" "$tmp_tpl"
  chmod 0644 "$tmp_tpl"
  mv -f "$tmp_tpl" "$PROJ/templates/$n" || { rm -f "$tmp_tpl"; fail "can't install templates/$n"; }
  log "installed templates/$n"
done
fetch_secrets
render_config
(cd "$PROJ" && cp "$src/project.xml" project.xml && bin/xadd)
# Stock update_versions has no app selector and can abort on an unrelated version's
# immutable files. Keep its code and signed payloads unchanged, but isolate its apps tree.
registration=$(mktemp -d)
mkdir -p "$registration/html" "$registration/apps/$APPVER_APP/$APPVER_VERSION"
cp -LR "$PROJ/html/inc" "$registration/html/inc"
ln -s "$PROJ/config.xml" "$registration/config.xml"
ln -s "$dst" "$registration/apps/$APPVER_APP/$APPVER_VERSION/$subdir"
(cd "$registration" && "$PROJ/bin/update_versions" --noconfirm)
# PHP die(string) exits zero; the job status alone is not evidence of registration.
(cd "$registration" && php <<'PHP'
<?php
chdir("html/inc");
require_once("boinc_db.inc");
$name = getenv("APPVER_APP");
$version = getenv("APPVER_VERSION");
$kind = getenv("APPVER_KIND") ?: "container";
$platformName = getenv("APPVER_PLATFORM") ?: "x86_64-pc-linux-gnu";
$plan = $kind === "native" ? (getenv("APPVER_PLAN_CLASS") ?: "") : (getenv("APPVER_PLAN_CLASS") ?: "docker");
if (!preg_match('/^usnm_[a-z_]+$/D', $name)
    || !preg_match('/^[1-9][0-9]*\.[0-9]+$/D', $version)
    || !in_array($platformName, ["x86_64-pc-linux-gnu", "aarch64-unknown-linux-gnu", "arm64-apple-darwin", "x86_64-apple-darwin", "windows_x86_64"], true)
    || !in_array($plan, ["", "docker", "docker_mt", "docker_nvidia_cuda", "docker_mt_2b", "docker_nvidia_cuda_8b", "docker_nvidia_train", "docker_intel_opencl_4b", "mt", "apple_gpu_8b", "nvidia_cuda_8b", "nvidia_cuda_2b"], true)) {
    fwrite(STDERR, "Invalid app-version verification inputs\n"); exit(1);
}
$allowed = $kind === "container"
    ? ["usnm_ndlocr:docker", "usnm_ocr:docker", "usnm_assess:docker", "usnm_gold:docker", "usnm_quality:docker",
       "usnm_surya:docker_mt", "usnm_surya:docker_nvidia_cuda",
       "usnm_qwen:docker_mt_2b", "usnm_qwen:docker_nvidia_cuda_8b", "usnm_qwen_train:docker_nvidia_train",
       "usnm_iris:docker_intel_opencl_4b"]
    : ($kind === "native"
        ? ["usnm_quality:", "usnm_qwen:mt", "usnm_qwen:apple_gpu_8b",
           "usnm_qwen:nvidia_cuda_8b", "usnm_qwen:nvidia_cuda_2b"] : []);
if (!in_array("$name:$plan", $allowed, true)
    || ($kind === "container" && $platformName !== "x86_64-pc-linux-gnu" && $plan !== "docker")) {
    fwrite(STDERR, "Unsupported app/plan class for verification\n"); exit(1);
}
$app = BoincApp::lookup("name='$name'");
$platform = BoincPlatform::lookup("name='$platformName'");
if (!$app || !$platform) {
    fwrite(STDERR, "App or platform registration is missing\n"); exit(1);
}
[$major, $minor] = array_map("intval", explode(".", $version));
$number = 100 * $major + $minor;
$versions = BoincAppVersion::enum("appid=$app->id and version_num=$number and platformid=$platform->id and plan_class='$plan'");
if (count($versions) != 1) {
    fwrite(STDERR, "Expected exactly one registered app version\n"); exit(1);
}
echo "Verified $name $version $platformName $plan (app_version ".$versions[0]->id.")\n";
PHP
)
touch "$PROJ/reread_db"
chown -R boincadm:boincadm "$dst" "$PROJ/templates" 2> /dev/null ||
  log "chown on the share failed (UNVERIFIED semantics, design.md 2.3)"
for f in "$dst"/*; do
  b=$(basename "$f")
  [ ! -e "$PROJ/download/$b" ] || chown boincadm:boincadm "$PROJ/download/$b" 2> /dev/null || true
done
log "published $APPVER_APP $APPVER_VERSION"
