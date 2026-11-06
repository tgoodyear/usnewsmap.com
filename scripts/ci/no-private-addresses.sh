#!/usr/bin/env bash
# Fail on any RFC 1918 private IPv4 address (the 10/8, 172.16/12 and
# 192.168/16 blocks) written out in a tracked file: code, scripts, Bicep,
# docs, READMEs and tests alike. Refer to a private host by its name, by a
# constant or environment variable, or best a constant that holds the name;
# tests use the RFC 5737 documentation ranges, or build a private address
# from its parts at run time. A standalone package pin in pip-freeze.txt is
# treated as a version, not an address.
#
#   no-private-addresses.sh [--repo DIR]
#
# --repo: the git checkout to scan (default: the one this script is in).
# Binary files are skipped. Exits 1 listing every match.
set -euo pipefail
usage() { echo "usage: no-private-addresses.sh [--repo DIR]" >&2; exit 2; }

REPO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO=${2:-}; shift 2 || usage ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$REPO" ] || REPO=$(cd "$(dirname "$0")" && git rev-parse --show-toplevel)

# A dotted quad (octets 0-255) in one of the three ranges, CIDR blocks
# included, but not inside a longer run of digits and dots (a version like
# 1.10.0.0.4).
o='(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])'
quad="10\\.$o\\.$o\\.$o|172\\.(1[6-9]|2[0-9]|3[01])\\.$o\\.$o|192\\.168\\.$o\\.$o"
pattern="(^|[^0-9.])($quad)([^0-9.]|\\.([^0-9]|$)|$)"

# git grep exits 1 for no match; anything above that (no repository, an
# unreadable file) fails the check rather than passing it unscanned.
rc=0
found=$(git -C "$REPO" grep -I -n -E "$pattern") || rc=$?
[ "$rc" -le 1 ] || { echo "error: git grep failed ($rc) in $REPO; nothing was scanned" >&2; exit 2; }
if [ -n "$found" ]; then
  # Do not suppress URLs, comments, or any other text on a matching line.
  rc=0
  found=$(printf '%s\n' "$found" | grep -Ev '(^|/)pip-freeze\.txt:[0-9]+:[A-Za-z0-9][A-Za-z0-9._-]*==[0-9]+(\.[0-9]+){3}$') || rc=$?
  [ "$rc" -le 1 ] || { echo "error: could not filter package pins ($rc)" >&2; exit 2; }
fi
if [ -n "$found" ]; then
  echo "Private (RFC 1918) addresses in tracked files. Use a host name, a constant or an environment variable instead:"
  echo "$found"
  exit 1
fi
echo "no private addresses"
