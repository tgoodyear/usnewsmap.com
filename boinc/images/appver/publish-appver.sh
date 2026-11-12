#!/usr/bin/env bash
# The publish job's command (infra/boinc/main.bicep, publishJob): copies the signed app version
# baked into this image (Containerfile) onto the project share, installs its templates, and runs
# the stock bin/update_versions, which reads the .sig files and adds the app_version rows.
#
# Platform versions are immutable: an existing platform directory must be identical,
# and then only update_versions runs again (it skips versions it already has).
#
# APPVER_HOLD=1 (boinc-app-release.sh --hold, #1687) publishes the version held: the row is
# inserted already deprecated, in update_versions' one INSERT, so no feeder reload (a restart,
# a pending reread_db) can ever load it, and a job that fails after the insert leaves it held.
# A journal under <project>/ops-app-versions/ records the publication first.
# app-version-hold.php --action release un-deprecates it later.
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
      usnm_ndlocr:docker|usnm_ocr:docker|usnm_ocr:docker_lines|usnm_assess:docker|usnm_gold:docker|usnm_gold:docker_lines|usnm_quality:docker|usnm_surya:docker_mt|usnm_surya:docker_nvidia_cuda|usnm_surya:docker_intel_vulkan|usnm_qwen:docker_mt_2b|usnm_qwen:docker_nvidia_cuda_8b|usnm_qwen_train:docker_nvidia_train|usnm_iris:docker_intel_opencl_4b) ;;
      *) fail "unsupported container app/plan class: $APPVER_APP:$APPVER_PLAN_CLASS" ;;
    esac
    case "$APPVER_PLATFORM:$APPVER_PLAN_CLASS" in
      x86_64-pc-linux-gnu:* | *:docker | *:docker_lines) ;;
      *) fail "$APPVER_PLATFORM supports only plan classes docker and docker_lines" ;;
    esac
    subdir="${APPVER_PLATFORM}__${APPVER_PLAN_CLASS}"
    ;;
  native)
    APPVER_PLAN_CLASS=${APPVER_PLAN_CLASS-}
    case "$APPVER_APP:$APPVER_PLAN_CLASS" in
      usnm_quality:|usnm_ocr_native:|usnm_gold_native:|usnm_qwen:mt|usnm_qwen:apple_gpu_8b|usnm_qwen:nvidia_cuda_8b|usnm_qwen:nvidia_cuda_2b|usnm_qwen:intel_vulkan_8b|usnm_qwen:nvidia_vulkan_8b|usnm_qwen:nvidia_vulkan_6gb_8b|usnm_qwen:nvidia_vulkan_2b|usnm_qwen:amd_vulkan_8b|usnm_qwen:mt_x08|usnm_qwen:apple_gpu_x08|usnm_qwen:nvidia_vulkan_x08|usnm_qwen:intel_vulkan_x08|usnm_qwen:amd_vulkan_x08) ;;
      *) fail "unsupported native app/plan class: $APPVER_APP:$APPVER_PLAN_CLASS" ;;
    esac
    case "$APPVER_PLAN_CLASS:$APPVER_PLATFORM" in
      :*|mt:*|apple_gpu_8b:arm64-apple-darwin|nvidia_cuda_8b:x86_64-pc-linux-gnu|nvidia_cuda_8b:aarch64-unknown-linux-gnu|nvidia_cuda_8b:windows_x86_64|nvidia_cuda_2b:x86_64-pc-linux-gnu|nvidia_cuda_2b:aarch64-unknown-linux-gnu|nvidia_cuda_2b:windows_x86_64|intel_vulkan_8b:x86_64-pc-linux-gnu|intel_vulkan_8b:windows_x86_64|nvidia_vulkan_8b:x86_64-pc-linux-gnu|nvidia_vulkan_8b:windows_x86_64|nvidia_vulkan_6gb_8b:x86_64-pc-linux-gnu|nvidia_vulkan_6gb_8b:windows_x86_64|nvidia_vulkan_2b:x86_64-pc-linux-gnu|amd_vulkan_8b:x86_64-pc-linux-gnu|amd_vulkan_8b:windows_x86_64|mt_x08:*|apple_gpu_x08:arm64-apple-darwin|nvidia_vulkan_x08:x86_64-pc-linux-gnu|nvidia_vulkan_x08:windows_x86_64|intel_vulkan_x08:x86_64-pc-linux-gnu|intel_vulkan_x08:windows_x86_64|amd_vulkan_x08:x86_64-pc-linux-gnu|amd_vulkan_x08:windows_x86_64) ;;
      *) fail "native plan class $APPVER_PLAN_CLASS is not supported on $APPVER_PLATFORM" ;;
    esac
    case "$APPVER_APP" in
      usnm_ocr_native|usnm_gold_native) [ "$APPVER_PLATFORM" = x86_64-pc-linux-gnu ] || fail "native $APPVER_APP is built only for x86_64-pc-linux-gnu" ;;
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
case "${APPVER_HOLD:-0}:$APPVER_KIND:$APPVER_APP:$APPVER_PLAN_CLASS" in
  0:*) ;;
  1:container:usnm_ocr:docker | 1:container:usnm_gold:docker | 1:container:usnm_ocr:docker_lines | 1:container:usnm_gold:docker_lines) ;;
  # Native Qwen (#1672): a version for a class with general admission is sent at once, so a new
  # platform goes out held until a real client has run it.
  1:native:usnm_qwen:?*) ;;
  1:*) fail "APPVER_HOLD is only for usnm_ocr and usnm_gold container versions and native usnm_qwen versions" ;;
  *) fail "APPVER_HOLD must be 0 or 1" ;;
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
private_directory() {
  python3 - "$1" "$2" <<'PYDIR'
from pathlib import Path
import sys
import uuid
path = Path(sys.argv[1]) / (sys.argv[2] + uuid.uuid4().hex)
path.mkdir(mode=0o700)
print(path)
PYDIR
}
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
  stage=$(private_directory "$version_dir" ".$subdir.")
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
  template_stage=$(private_directory "$PROJ/templates" ".$n.")
  tmp_tpl="$template_stage/template"
  cp "$t" "$tmp_tpl"
  chmod 0644 "$tmp_tpl"
  mv -f "$tmp_tpl" "$PROJ/templates/$n" || { rm -f "$tmp_tpl"; fail "can't install templates/$n"; }
  rmdir "$template_stage"
  log "installed templates/$n"
done
case "${APPVER_REUSE_PROJECT:-0}" in
  0)
    fetch_secrets
    render_config
    # xadd, then the declared weight of each app it added: xadd alone leaves a new app at
    # weight 1 (register-apps.php, docs/boinc/server/bridge-steps.md).
    (cd "$PROJ" && cp "$src/project.xml" project.xml && php /usr/local/lib/usnm-boinc/register-apps.php)
    ;;
  1)
    # Scoped replay uses the live project's existing credentials/configuration and app rows.
    [ -s "$PROJ/html/inc/boinc_db.inc" ] || fail "scoped publication requires an existing project"
    fetch_secrets
    # A shared config symlink points into each container's own tmpfs, not the web replica's.
    # Registration needs DB/download settings only; never re-render the live scheduler config.
    python3 - "$SECRETS/appver-config.xml" "$SECRETS/db-runtime" "$PROJ" <<'PYCONFIG'
import os, sys
import xml.etree.ElementTree as ET
root = ET.Element("boinc")
config = ET.SubElement(root, "config")
for key, value in {
    "db_host": os.environ["DB_HOST"], "db_name": os.environ["DB_NAME"],
    "db_user": os.environ["DB_RUNTIME_USER"],
    "db_passwd": open(sys.argv[2]).read().rstrip("\n"),
    "project_dir": sys.argv[3], "download_dir": sys.argv[3] + "/download",
    "download_url": "https://" + os.environ["MASTER_HOST"] + "/" + os.environ.get("PROJECT", "usnmocr") + "_download",
    "key_dir": os.path.dirname(sys.argv[1]) + "/keys",
}.items():
    ET.SubElement(config, key).text = value
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "wb") as stream:
    ET.ElementTree(root).write(stream, encoding="utf-8")
PYCONFIG
    ;;
  *) fail "APPVER_REUSE_PROJECT must be 0 or 1" ;;
esac
# Stock update_versions has no app selector and can abort on an unrelated version's
# immutable files. Keep its code and signed payloads unchanged, but isolate its apps tree.
# Azure Files forces shared directory modes; executable PHP must stay on tmpfs.
registration=$(private_directory "$SECRETS" "appver-registration.")
python3 - "$registration" <<'PYPRIVATE'
import os
import sys
info = os.stat(sys.argv[1], follow_symlinks=False)
if info.st_mode & 0o777 != 0o700 or info.st_uid != os.geteuid():
    raise PermissionError("registration storage must enforce private owner-only permissions")
PYPRIVATE
mkdir -p "$registration/html" "$registration/apps/$APPVER_APP/$APPVER_VERSION"
cp -LR "$PROJ/html/inc" "$registration/html/inc"
if [ "${APPVER_REUSE_PROJECT:-0}" = 1 ]; then
  ln -s "$SECRETS/appver-config.xml" "$registration/config.xml"
  ln -s "$SECRETS/keys" "$registration/keys"
else
  ln -s "$PROJ/config.xml" "$registration/config.xml"
fi
ln -s "$dst" "$registration/apps/$APPVER_APP/$APPVER_VERSION/$subdir"
update_versions="$PROJ/bin/update_versions"
if [ "${APPVER_HOLD:-0}" = 1 ]; then
  # A private copy whose one app_version INSERT sets deprecated=1: the row is born held. Exactly
  # one INSERT must match, or the stock script changed and this refuses before registering.
  update_versions="$registration/update_versions.held"
  python3 - "$PROJ/bin/update_versions" "$update_versions" <<'PYHOLD' || fail "can't make update_versions register the version held; nothing registered"
import os, sys
text = open(sys.argv[1]).read()
needle = 'xml_doc=\'$xml\', plan_class=\'$plan_class\', beta=$b";'
if text.count(needle) != 1 or text.count("BoincAppVersion::insert(") != 1:
    raise SystemExit("update_versions has no single app_version insert to hold")
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
with os.fdopen(fd, "w") as stream:
    stream.write(text.replace(needle, needle[:-2] + ', deprecated=1";'))
PYHOLD
  mkdir -p "$PROJ/ops-app-versions"
  journal="$PROJ/ops-app-versions/publish-held-$APPVER_APP-$APPVER_VERSION-$APPVER_PLATFORM-$(date -u +%Y%m%dT%H%M%SZ)-$$.json"
  printf '{"action": "publish-held", "app": "%s", "deprecated": 1, "plan_class": "%s", "platform": "%s", "version": "%s"}\n' \
    "$APPVER_APP" "$APPVER_PLAN_CLASS" "$APPVER_PLATFORM" "$APPVER_VERSION" > "$journal" ||
    fail "can't write the publication journal; nothing registered"
fi
if [ "${APPVER_HOLD:-0}" = 1 ]; then
  # The held copy sits in the registration directory, which can be on a noexec mount (/dev/shm in
  # the server container, 10 Nov): run it through the interpreter the stock script's shebang names.
  shebang=$(head -n 1 "$PROJ/bin/update_versions")
  case "$shebang" in
    '#!/usr/bin/env php' | '#! /usr/bin/env php') interpreter=php ;;
    '#!/'*php | '#! /'*php) interpreter=${shebang#'#!'}; interpreter=${interpreter# } ;;
    *) fail "update_versions isn't a PHP script ($shebang); nothing registered" ;;
  esac
  (cd "$registration" && "$interpreter" "$update_versions" --noconfirm)
else
  (cd "$registration" && "$update_versions" --noconfirm)
fi
# PHP die(string) exits zero; the job status alone is not evidence of registration.
verified=$(cd "$registration" && php <<'PHP'
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
    || !in_array($plan, ["", "docker", "docker_lines", "docker_mt", "docker_nvidia_cuda", "docker_intel_vulkan", "docker_mt_2b", "docker_nvidia_cuda_8b", "docker_nvidia_train", "docker_intel_opencl_4b", "mt", "apple_gpu_8b", "nvidia_cuda_8b", "nvidia_cuda_2b", "intel_vulkan_8b", "nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_2b", "amd_vulkan_8b", "mt_x08", "apple_gpu_x08", "nvidia_vulkan_x08", "intel_vulkan_x08", "amd_vulkan_x08"], true)) {
    fwrite(STDERR, "Invalid app-version verification inputs\n"); exit(1);
}
$allowed = $kind === "container"
    ? ["usnm_ndlocr:docker", "usnm_ocr:docker", "usnm_ocr:docker_lines", "usnm_assess:docker", "usnm_gold:docker",
       "usnm_gold:docker_lines", "usnm_quality:docker",
       "usnm_surya:docker_mt", "usnm_surya:docker_nvidia_cuda", "usnm_surya:docker_intel_vulkan", "usnm_surya:intel_vulkan_8b",
       "usnm_qwen:docker_mt_2b", "usnm_qwen:docker_nvidia_cuda_8b", "usnm_qwen_train:docker_nvidia_train",
       "usnm_iris:docker_intel_opencl_4b"]
    : ($kind === "native"
        ? ["usnm_quality:", "usnm_ocr_native:", "usnm_gold_native:", "usnm_qwen:mt", "usnm_qwen:apple_gpu_8b",
           "usnm_qwen:nvidia_cuda_8b", "usnm_qwen:nvidia_cuda_2b", "usnm_qwen:intel_vulkan_8b", "usnm_qwen:nvidia_vulkan_8b", "usnm_qwen:nvidia_vulkan_6gb_8b", "usnm_qwen:nvidia_vulkan_2b", "usnm_qwen:amd_vulkan_8b",
           "usnm_qwen:mt_x08", "usnm_qwen:apple_gpu_x08", "usnm_qwen:nvidia_vulkan_x08", "usnm_qwen:intel_vulkan_x08", "usnm_qwen:amd_vulkan_x08"] : []);
if (!in_array("$name:$plan", $allowed, true)
    || ($kind === "container" && $platformName !== "x86_64-pc-linux-gnu" && $plan !== "docker" && $plan !== "docker_lines")) {
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
// A held publication (#1687) must have registered the row deprecated. A version registered live
// earlier (a rerun of a release made without --hold) stays as it is: hold it by hand.
if (getenv("APPVER_HOLD") === "1" && intval($versions[0]->deprecated) !== 1) {
    fwrite(STDERR, "The registered app version ".$versions[0]->id." is live, not held; hold it with app-version-hold.php\n"); exit(1);
}
echo "Verified $name $version $platformName $plan (app_version ".$versions[0]->id.")\n";
PHP
)
echo "$verified"
[ "${APPVER_HOLD:-0}" != 1 ] || log "published $APPVER_APP $APPVER_VERSION held (deprecated from its insert)"
touch "$PROJ/reread_db"
ownership_paths=("$dst" "$PROJ/templates")
# Scoped publication must not walk the unrelated live download tree on Azure Files.
if [ "${APPVER_REUSE_PROJECT:-0}" = 0 ]; then
  ownership_paths+=("$PROJ/download")
fi
chown -R boincadm:boincadm "${ownership_paths[@]}" 2> /dev/null ||
  log "chown on the share failed (UNVERIFIED semantics, design.md 2.3)"
log "published $APPVER_APP $APPVER_VERSION"
