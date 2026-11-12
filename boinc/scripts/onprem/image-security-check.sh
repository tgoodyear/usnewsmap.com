#!/usr/bin/env bash
# Checks the image the on-prem BOINC server runs against Debian's security archive, and the
# MySQL image's pin against its tag (#969). usnm-image-security-check.timer runs it on the BOINC
# host; the failed unit and its journal lines are the alert.
#
#   image-security-check.sh --php-floor VERSION --apache-floor VERSION
#     [--server-env /etc/usnm-boinc/server.env] [--image IMAGE]
#     [--mysql-unit /etc/systemd/system/usnm-boinc-mysql.service]
#     [--mysql-image docker.io/library/mysql:8.4] [--mysql-floor VERSION] [--skip-mysql]
#
# The image is SERVER_IMAGE from --server-env, or --image. It is a local build, so it is never
# pulled: a throwaway container from it (podman run --rm, host network) runs apt-get update and
# a simulated apt-get upgrade, then compares php8.2-common and apache2 with the two floors
# (the PHP_FLOOR and APACHE_FLOOR of the Containerfiles). It changes nothing in the image.
#
# A finding is any of: an upgrade the image's own apt sources offer (SECURITY when the source is
# the security archive, UPDATE otherwise, since a security fix can also arrive in a point
# release); a package apt-get upgrade keeps back because upgrading it needs a new package or a
# removal (KEPT-BACK; apt prints no Inst line for it, so it is read from the "not upgraded"
# count); php8.2-common or apache2 older than its floor; or MySQL behind its tag (below). The
# MySQL findings are only reported: rolling the database restarts it
# (docs/boinc/server/onprem.md, "Security updates").
#
# MySQL: the digest baked into --mysql-unit is compared with what --mysql-image resolves to now
# (podman pull, as mysql-setup.sh does). Oracle rebuilds the tag on a base-image refresh without
# a new MySQL release, so a new digest alone is an informational line (MYSQL-REBUILD), not a
# failure. These fail:
#   MYSQL-DRIFT       the tag's MYSQL_VERSION (an environment variable of the image) differs from
#                     the pinned image's, or either version can't be read, so a same-version
#                     rebuild can't be shown;
#   MYSQL-BELOW-FLOOR --mysql-floor is given and the pinned image's MYSQL_VERSION is older than
#                     it. Set it from a MySQL security advisory (a Critical Patch Update) that
#                     the pinned version predates; a rebuild of the same version doesn't clear
#                     it, so the unit stays failed until the database is rolled onto a fixed
#                     version. Without --mysql-floor there is no such check.
#
# --mysql-image must be a name with a tag, as the default is (a registry port is fine). The check
# asks what the tag resolves to now, so a name with no tag (podman pulls :latest) or a reference
# with an @digest (podman returns that digest) is refused as a bad argument.
#
# Exit status: 0 nothing found; 1 a finding; 2 bad arguments; 3 a check could not run (no
# network, no security source in the image's apt sources, no digest in the unit).
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
bad_args() { echo "error: $*" >&2; exit 2; }
log() { echo "usnm-image-security-check: $*"; }

PHP_FLOOR="" APACHE_FLOOR="" SERVER_ENV=/etc/usnm-boinc/server.env IMAGE=""
MYSQL_UNIT=/etc/systemd/system/usnm-boinc-mysql.service MYSQL_IMAGE=docker.io/library/mysql:8.4 SKIP_MYSQL=false
MYSQL_FLOOR=""
while [ $# -gt 0 ]; do
  case $1 in
    --php-floor | --apache-floor | --server-env | --image | --mysql-unit | --mysql-image | --mysql-floor)
      [ $# -ge 2 ] || bad_args "$1 needs a value"
      case $1 in
        --php-floor) PHP_FLOOR=$2 ;;
        --apache-floor) APACHE_FLOOR=$2 ;;
        --server-env) SERVER_ENV=$2 ;;
        --image) IMAGE=$2 ;;
        --mysql-unit) MYSQL_UNIT=$2 ;;
        --mysql-image) MYSQL_IMAGE=$2 ;;
        --mysql-floor) MYSQL_FLOOR=$2 ;;
      esac
      shift 2 ;;
    --skip-mysql) SKIP_MYSQL=true; shift ;;
    -h | --help) usage ;;
    *) bad_args "unknown argument: $1 (see --help)" ;;
  esac
done
version_re='^[0-9][A-Za-z0-9.+~:-]{0,80}$'
image_re='^[A-Za-z0-9._/:@-]{1,200}$'
# The last path component of a MySQL image reference, name and tag: mysql:8.4.
last_component_tagged_re='^[^:]+:[A-Za-z0-9_][A-Za-z0-9._-]{0,127}$'
[[ $PHP_FLOOR =~ $version_re ]] || bad_args "--php-floor: a Debian version, such as 8.2.35-1~deb12u1"
[[ $APACHE_FLOOR =~ $version_re ]] || bad_args "--apache-floor: a Debian version, such as 2.4.67-1~deb12u3"
[[ -z $MYSQL_FLOOR || $MYSQL_FLOOR =~ $version_re ]] || bad_args "--mysql-floor: a MySQL version, such as 8.4.13-1.el9"
[[ -z $IMAGE || $IMAGE =~ $image_re ]] || bad_args "--image: an image name or sha256:<id>"
[[ $MYSQL_IMAGE =~ $image_re ]] || bad_args "--mysql-image: an image name made only of letters, digits and . _ / : - (up to 200), such as docker.io/library/mysql:8.4"
# A colon in the last path component starts the tag; earlier, it is a registry port.
[[ $MYSQL_IMAGE != *@* ]] || bad_args "--mysql-image: a reference with a digest can't show a moved tag; give an image name with a tag"
[[ ${MYSQL_IMAGE##*/} =~ $last_component_tagged_re ]] || bad_args "--mysql-image: an image name with a tag, such as docker.io/library/mysql:8.4"

findings=0 errors=0
finding() { findings=$((findings + 1)); log "$*"; }
cannot() { errors=$((errors + 1)); log "COULD-NOT-CHECK $*"; }

# The name a unit pins with @sha256:..., from an image reference: no :tag. A colon is a tag only in
# the last path component; earlier, it is a registry port, so
# registry.example:5000/library/mysql:8.4 gives registry.example:5000/library/mysql and
# registry.example:5000/library/mysql (no tag) is left as it is.
image_name() {
  if [[ ${1##*/} == *:* ]]; then printf '%s\n' "${1%:*}"; else printf '%s\n' "$1"; fi
}

# The packages. The script runs in a container from the image, as root, and prints
# PENDING lines (apt's own Inst lines), one HELD line (the "not upgraded" count, then the names
# apt lists as kept back) and FLOOR lines (package, installed, floor, ok|below).
# Anything that goes wrong stops it with a message on stderr, so a failed apt-get update can't
# read as nothing pending.
read -r -d '' INNER <<'SH' || true
set -eu
export LC_ALL=C DEBIAN_FRONTEND=noninteractive
apt-get update -qq -o APT::Update::Error-Mode=any >&2
apt-cache policy | grep -q -- '-security' || { echo "no security archive in this image's apt sources" >&2; exit 4; }
simulated=$(apt-get -s -q upgrade)
printf '%s\n' "$simulated" | sed -n 's/^Inst /PENDING /p'
# A package whose upgrade needs a new package or a removal is kept back, with no Inst line. Only
# the summary's count, and the list above it, show it. Without the count, nothing says none were.
held=$(printf '%s\n' "$simulated" | sed -n 's/^.* \([0-9][0-9]*\) not upgraded\.$/\1/p')
[ -n "$held" ] || { echo "apt-get upgrade printed no 'not upgraded' count" >&2; exit 4; }
names=$(printf '%s\n' "$simulated" | sed -n '/^The following packages have been kept back:$/,/^[^ ]/{/^ /p;}' | tr -s ' \n' '  ')
echo "HELD $held $names"
floor() {
  installed=$(dpkg-query -W -f='${Version}' "$1")
  if dpkg --compare-versions "$installed" ge "$2"; then state=ok; else state=below; fi
  echo "FLOOR $1 $installed $2 $state"
}
floor php8.2-common "$PHP_FLOOR"
floor apache2 "$APACHE_FLOOR"
SH

if [ -z "$IMAGE" ] && [ -r "$SERVER_ENV" ]; then
  # An EnvironmentFile: the last assignment wins.
  IMAGE=$(sed -n 's/^SERVER_IMAGE=\([A-Za-z0-9._/:@-]\{1,200\}\)$/\1/p' "$SERVER_ENV" | tail -n 1)
fi
if [ -z "$IMAGE" ]; then
  cannot "no SERVER_IMAGE in $SERVER_ENV, and no --image"
else
  log "checking $IMAGE (floors: php8.2-common $PHP_FLOOR, apache2 $APACHE_FLOOR)"
  if report=$(podman run --rm --pull=never --network host --entrypoint sh \
      -e "PHP_FLOOR=$PHP_FLOOR" -e "APACHE_FLOOR=$APACHE_FLOOR" "$IMAGE" -c "$INNER"); then
    # Inst pkg [installed] (candidate origin[, origin] [arch]), then ` []` when apt lists what the
    # upgrade leaves to configure.
    pending_re='^PENDING ([^ ]+) \[([^]]+)\] \(([^ ]+) (.*) \[[^]]+\]\)( \[[^]]*\])?$'
    floors_seen=0
    while IFS= read -r line; do
      case $line in
        "PENDING "*)
          if [[ $line =~ $pending_re ]]; then
            package=${BASH_REMATCH[1]} installed=${BASH_REMATCH[2]} candidate=${BASH_REMATCH[3]} source=${BASH_REMATCH[4]}
            kind=UPDATE
            [[ $source == *[Ss]ecurity* ]] && kind=SECURITY
            finding "$kind $package $installed -> $candidate from $source"
          else
            kind=UPDATE
            [[ $line == *[Ss]ecurity* ]] && kind=SECURITY
            finding "$kind (apt line not understood) ${line#PENDING }"
          fi ;;
        "HELD "*)
          read -r _ held names <<< "$line"
          if [ "$held" -gt 0 ]; then
            finding "KEPT-BACK $held package(s) apt-get upgrade will not upgrade, as that needs a new package or a removal${names:+: $names}"
          fi ;;
        "FLOOR "*)
          read -r _ package installed floor state <<< "$line"
          floors_seen=$((floors_seen + 1))
          if [ "$state" = below ]; then
            finding "BELOW-FLOOR $package $installed is older than $floor"
          else
            log "$package $installed meets $floor"
          fi ;;
      esac
    done <<< "$report"
    [ "$floors_seen" -eq 2 ] || cannot "the image reported $floors_seen of 2 package floors"
  else
    cannot "podman run from $IMAGE failed (exit $?): see the lines above"
  fi
fi

# MySQL: the digest the unit runs against the digest the tag has now. podman pull stores the
# new image if the tag moved, which is the image the manual roll uses next.
# MYSQL_VERSION of an image, from its environment, on stdout. Fails when podman does, or when the
# image has none.
image_mysql_version() {
  local env version
  env=$(podman image inspect "$1" --format '{{range .Config.Env}}{{println .}}{{end}}') || return 1
  version=$(printf '%s\n' "$env" | sed -n 's/^MYSQL_VERSION=//p' | tail -n 1)
  [[ $version =~ $version_re ]] || return 1
  printf '%s\n' "$version"
}
# Whether version $1 is at least version $2, in sort -V order.
version_at_least() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$2" ]; }

if [ "$SKIP_MYSQL" = false ]; then
  digest_re='^sha256:[0-9a-f]{64}$'
  mysql_name=$(image_name "$MYSQL_IMAGE")
  pinned=""
  if [ -r "$MYSQL_UNIT" ]; then
    pinned=$(grep -o "$mysql_name@sha256:[0-9a-f]\{64\}" "$MYSQL_UNIT" | sed 's/.*@//' | sort -u || true)
  fi
  if ! [[ $pinned =~ $digest_re ]]; then
    cannot "$MYSQL_UNIT has no single $mysql_name@sha256 digest"
  elif ! podman pull -q "$MYSQL_IMAGE" > /dev/null; then
    cannot "podman pull $MYSQL_IMAGE failed"
  elif ! current=$(podman image inspect "$MYSQL_IMAGE" --format '{{.Digest}}'); then
    cannot "podman image inspect $MYSQL_IMAGE failed"
  elif ! [[ $current =~ $digest_re ]]; then
    cannot "podman image inspect $MYSQL_IMAGE gave no digest"
  else
    pinned_version="" tag_version=""
    if pinned_version=$(image_mysql_version "$mysql_name@$pinned"); then
      if [ -n "$MYSQL_FLOOR" ] && ! version_at_least "$pinned_version" "$MYSQL_FLOOR"; then
        finding "MYSQL-BELOW-FLOOR the unit runs MySQL $pinned_version ($pinned), older than the floor $MYSQL_FLOOR (a roll restarts the database)"
      fi
    elif [ -n "$MYSQL_FLOOR" ]; then
      cannot "no MYSQL_VERSION for the pinned image $mysql_name@$pinned, to compare with the floor $MYSQL_FLOOR"
    fi
    if [ "$current" = "$pinned" ]; then
      log "MySQL $MYSQL_IMAGE is still $pinned"
    else
      tag_version=$(image_mysql_version "$MYSQL_IMAGE") || tag_version=""
      if [ -n "$pinned_version" ] && [ "$tag_version" = "$pinned_version" ]; then
        # A rebuild: new digest, same MySQL release. Reported so it is on the record, but it is
        # not a fix to MySQL itself, so it doesn't fail the unit.
        log "MYSQL-REBUILD $MYSQL_IMAGE is $current now, a rebuild of MySQL $tag_version; the unit runs $pinned, the same version (informational)"
      else
        finding "MYSQL-DRIFT $MYSQL_IMAGE is $current now (MySQL ${tag_version:-unknown}); the unit runs $pinned (MySQL ${pinned_version:-unknown}) (a roll restarts the database)"
      fi
    fi
  fi
fi

if [ "$findings" -gt 0 ]; then
  log "FAILED: $findings finding(s). Roll the image or the database as in docs/boinc/server/onprem.md, \"Security updates\"."
  exit 1
elif [ "$errors" -gt 0 ]; then
  log "FAILED: $errors check(s) could not run."
  exit 3
fi
if [ "$SKIP_MYSQL" = true ]; then
  log "OK: nothing pending for $IMAGE, and its floors hold (MySQL skipped)."
else
  log "OK: nothing pending for $IMAGE, and its floors and the MySQL pin hold."
fi
